//! Cargo runner. Native test verdicts require complete execution evidence.
use crate::platform::{self, Firmware, Machine};
use crate::{Result, output, process, require};
use serde::Serialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn variable(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.into())
}

fn command(machine: &Machine, image: &Path, directory: &Path, observe: bool) -> Result<Command> {
    let architecture = machine.architecture;
    let mut command = Command::new(match architecture {
        "riscv64" => variable("QEMU_RISCV64", "qemu-system-riscv64"),
        "aarch64" => variable("QEMU_AARCH64", "qemu-system-aarch64"),
        "x86_64" => variable("QEMU_X86_64", "qemu-system-x86_64"),
        _ => return Err("unsupported architecture".into()),
    });
    let count = machine.platform.harts.to_string();
    let accel = if architecture == "riscv64" {
        "tcg,thread=multi"
    } else {
        "tcg"
    };
    command.args([
        "-accel",
        accel,
        "-smp",
        &count,
        "-display",
        "none",
        "-monitor",
        "none",
        "-nic",
        "none",
        "-no-reboot",
    ]);
    command.args(["-machine", &machine.board]);
    if let Some(cpu) = machine.cpu {
        command.args(["-cpu", cpu]);
    }
    // Semihosting diagnostics must not share the QMP control stream.
    let semihost = if observe && architecture != "x86_64" {
        command.arg("-chardev").arg(format!(
            "file,id=semihost,path={}",
            directory.join("semihost.log").display()
        ));
        "enable=on,target=native,userspace=off,chardev=semihost"
    } else {
        "enable=on,target=native,userspace=off"
    };
    if machine.platform.firmware == Firmware::OpenSbi {
        let firmware = PathBuf::from(variable(
            "QEMU_OPENSBI",
            "/usr/lib/riscv64-linux-gnu/opensbi/generic/fw_dynamic.bin",
        ));
        crate::json(
            &directory.join("firmware.json"),
            &serde_json::json!({
                "code": firmware,
                "code_sha256": crate::digest(&firmware)?,
            }),
        )?;
        command
            .args(["-m", "128M", "-bios"])
            .arg(firmware)
            .arg("-kernel")
            .arg(image)
            .args(["-semihosting-config", semihost]);
        peripherals(&mut command, machine, directory, observe);
        return Ok(command);
    }

    let (firmware, code, variables, boot) = match machine.platform.firmware {
        Firmware::Ovmf => (
            variable("OVMF_DIR", "/usr/share/OVMF"),
            "OVMF_CODE_4M.fd",
            "OVMF_VARS_4M.fd",
            "BOOTX64.EFI",
        ),
        Firmware::Aavmf => (
            variable("AAVMF_DIR", "/usr/share/AAVMF"),
            "AAVMF_CODE.fd",
            "AAVMF_VARS.fd",
            "BOOTAA64.EFI",
        ),
        Firmware::OvmfStrictNx => (
            variable("OVMF_DIR", "/usr/share/OVMF"),
            "OVMF_CODE_4M.secboot.strictnx.fd",
            "OVMF_VARS_4M.fd",
            "BOOTX64.EFI",
        ),
        Firmware::AavmfStrictNx => (
            variable("AAVMF_DIR", "/usr/share/AAVMF"),
            "AAVMF_CODE.secboot.strictnx.fd",
            "AAVMF_VARS.fd",
            "BOOTAA64.EFI",
        ),
        Firmware::OpenSbi => unreachable!(),
    };
    let firmware = Path::new(&firmware);
    let esp = directory.join("esp");
    let boot_dir = esp.join("EFI/BOOT");
    fs::create_dir_all(&boot_dir)?;
    fs::copy(image, boot_dir.join(boot))?;
    let code = firmware.join(code);
    require(
        code.is_file(),
        format!("missing firmware image: {}", code.display()),
    )?;
    let private_vars = directory.join("vars.fd");
    fs::copy(firmware.join(variables), &private_vars)?;
    crate::json(
        &directory.join("firmware.json"),
        &serde_json::json!({
            "code": code,
            "code_sha256": crate::digest(&code)?,
            "initial_vars_sha256": crate::digest(&private_vars)?,
        }),
    )?;
    command
        .args(["-nodefaults", "-drive"])
        .arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            code.display()
        ))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,file={}",
            private_vars.display()
        ));
    if architecture == "x86_64" {
        command
            .args(["-m", "128M", "-drive"])
            .arg(format!("format=raw,file=fat:rw:{}", esp.display()))
            .arg("-debugcon")
            .arg(if observe {
                format!("file:{}", directory.join("debugcon.log").display())
            } else {
                "stdio".into()
            })
            .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=4"]);
    } else {
        command
            .args(["-m", "256M", "-drive"])
            .arg(format!(
                "if=none,id=esp,format=raw,file=fat:rw:{}",
                esp.display()
            ))
            .args(["-device", "virtio-blk-pci,drive=esp"])
            .args(["-semihosting-config", semihost]);
    }
    peripherals(&mut command, machine, directory, observe);
    Ok(command)
}

fn peripherals(command: &mut Command, machine: &Machine, directory: &Path, observe: bool) {
    if machine.architecture != "x86_64" {
        command.arg("-serial").arg(if observe {
            format!("file:{}", directory.join("serial.log").display())
        } else {
            "stdio".into()
        });
        if machine.secondary_serial {
            command.args(["-serial", "null"]);
        }
    }
    for device in &machine.devices {
        command.args(["-device", device]);
    }
}

/// Construct and record the selected machine without starting it.
pub fn prepare(image: &Path, directory: &Path, observe: bool) -> Result<Command> {
    let machine = platform::read(image)?;
    crate::json(
        &directory.join("platform.json"),
        &serde_json::json!({
            "architecture": machine.architecture,
            "firmware": format!("{:?}", machine.platform.firmware),
            "interrupt": format!("{:?}", machine.platform.interrupt),
            "harts": machine.platform.harts,
            "machine": machine.board,
            "cpu": machine.cpu,
            "iommu": format!("{:?}", machine.platform.iommu),
            "serial": format!("{:?}", machine.platform.serial),
            "devices": machine.devices,
            "image_sha256": crate::digest(image)?,
        }),
    )?;
    let command = command(&machine, image, directory, observe)?;
    let version = output(Command::new(command.get_program()).arg("--version"))?;
    require(
        version
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(3))
            == Some(include_str!("../qemu.version").trim()),
        "unexpected QEMU version",
    )?;
    crate::json(
        &directory.join("tools.json"),
        &serde_json::json!({
            "qemu": version,
            "runner_sha256": crate::digest(&env::current_exe()?)?,
        }),
    )?;
    Ok(command)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Verdict {
    Pass,
    Fail,
    Timeout,
    Inconclusive,
}

#[derive(Debug, Serialize)]
struct Report {
    verdict: Verdict,
    reason: String,
}

impl Report {
    fn new(verdict: Verdict, reason: impl Into<String>) -> Self {
        Self {
            verdict,
            reason: reason.into(),
        }
    }
}

fn judge(outcome: &process::Outcome, observation_complete: bool) -> Report {
    use process::Termination;
    if outcome.termination == Termination::Timeout {
        return Report::new(Verdict::Timeout, "execution deadline reached");
    }
    if !outcome.clean() || !observation_complete {
        return Report::new(
            Verdict::Inconclusive,
            "incomplete or conflicting execution and retirement evidence",
        );
    }
    match outcome.termination {
        Termination::Exited(97) => Report::new(Verdict::Pass, "explicit guest completion"),
        Termination::Exited(99) => Report::new(Verdict::Fail, "explicit guest failure"),
        Termination::Exited(_) | Termination::Signaled(_) => {
            Report::new(Verdict::Fail, "guest terminated without explicit completion")
        }
        _ => Report::new(Verdict::Inconclusive, "unknown termination"),
    }
}

fn execute(image: &Path, directory: &Path, observe: bool) -> Result<Report> {
    let mut command = prepare(image, directory, observe)?;
    let timeout = Duration::from_secs(45);
    if observe {
        let (outcome, shutdown) = crate::qmp::run_isolated(command, directory, timeout, None)?;
        let complete = match shutdown {
            Ok(shutdown) => {
                let consistent = shutdown.guest
                    && (outcome.termination != process::Termination::Exited(97)
                        || shutdown.reason == "guest-shutdown");
                crate::json(&directory.join("shutdown.json"), &shutdown)?;
                consistent
            }
            Err(error) => {
                fs::write(directory.join("qmp-error.txt"), error.to_string())?;
                false
            }
        };
        Ok(judge(&outcome, complete))
    } else {
        let outcome = process::run(&mut command, directory, timeout)?;
        Ok(judge(&outcome, true))
    }
}

pub fn run(image: &Path, observe: bool, evidence_root: Option<&Path>) -> Result<()> {
    let image = image.canonicalize()?;
    let evidence = match evidence_root {
        Some(root) => root.join(image.file_name().ok_or("missing executable name")?),
        None => PathBuf::from(env::var_os("LIMEN_TEST_EVIDENCE").ok_or("missing evidence path")?),
    };
    crate::fresh(&evidence)?;
    let directory = evidence.canonicalize()?;
    let path = directory.join("result.json");
    // An interrupted attempt cannot retain a successful verdict from an earlier run.
    crate::json(
        &path,
        &Report::new(Verdict::Inconclusive, "execution incomplete"),
    )?;
    let report = execute(&image, &directory, observe)
        .unwrap_or_else(|error| Report::new(Verdict::Inconclusive, error.to_string()));
    crate::json(&path, &report)?;
    require(
        report.verdict == Verdict::Pass,
        format!("native guest verdict: {report:?}"),
    )
}

#[cfg(test)]
#[path = "tests/native.rs"]
mod tests;

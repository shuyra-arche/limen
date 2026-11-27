//! Experimental Cargo runner. Only the dedicated machine exit code is success.
use crate::platform::{self, Firmware, Machine};
use kernel_verification::{Result, output, process, require};
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
        command
            .args(["-m", "128M", "-bios"])
            .arg(variable(
                "QEMU_OPENSBI",
                "/usr/lib/riscv64-linux-gnu/opensbi/generic/fw_dynamic.bin",
            ))
            .arg("-kernel")
            .arg(image)
            .args(["-semihosting-config", semihost]);
        if !observe {
            command.args(["-serial", "stdio"]);
        }
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
        Firmware::OpenSbi => unreachable!(),
    };
    let firmware = Path::new(&firmware);
    let esp = directory.join("esp");
    let boot_dir = esp.join("EFI/BOOT");
    fs::create_dir_all(&boot_dir)?;
    fs::copy(image, boot_dir.join(boot))?;
    let private_vars = directory.join("vars.fd");
    fs::copy(firmware.join(variables), &private_vars)?;
    command
        .args(["-nodefaults", "-drive"])
        .arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            firmware.join(code).display()
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
        if !observe {
            command.args(["-serial", "stdio"]);
        }
    }
    Ok(command)
}

pub fn run(image: &Path, observe: bool, evidence_root: Option<&Path>) -> Result<()> {
    let image = image.canonicalize()?;
    let evidence = match evidence_root {
        Some(root) => root.join(image.file_name().ok_or("missing executable name")?),
        None => PathBuf::from(env::var_os("LIMEN_T0_EVIDENCE").ok_or("missing evidence path")?),
    };
    kernel_verification::fresh(&evidence)?;
    let directory = evidence.canonicalize()?;
    let machine = platform::read(&image)?;
    kernel_verification::json(
        &directory.join("platform.json"),
        &serde_json::json!({
            "architecture": machine.architecture,
            "firmware": format!("{:?}", machine.platform.firmware),
            "interrupt": format!("{:?}", machine.platform.interrupt),
            "harts": machine.platform.harts,
            "machine": machine.board,
            "cpu": machine.cpu,
            "image_sha256": kernel_verification::digest(&image)?,
        }),
    )?;
    let mut command = command(&machine, &image, &directory, observe)?;
    let version = output(Command::new(command.get_program()).arg("--version"))?;
    require(
        version
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(3))
            == Some(include_str!("../qemu.version").trim()),
        "unexpected QEMU version",
    )?;
    let timeout = Duration::from_secs(45);
    let mut outcome = if observe {
        // External observation is optional; ordinary Cargo tests need only
        // the dedicated completion status.
        let (outcome, shutdown) = crate::qmp::run_isolated(command, &directory, timeout)?;
        match shutdown {
            Ok(shutdown) => {
                kernel_verification::json(&directory.join("shutdown.json"), &shutdown)?;
            }
            Err(error) => {
                fs::write(directory.join("qmp-error.txt"), error.to_string())?;
                return Err(error.into());
            }
        }
        outcome
    } else {
        process::run(&mut command, &directory, timeout)?
    };
    outcome.passed = outcome.exited() && outcome.code == Some(97);
    kernel_verification::json(&directory.join("result.json"), &outcome)?;
    require(
        outcome.passed,
        format!("no explicit test success: {outcome:?}"),
    )
}

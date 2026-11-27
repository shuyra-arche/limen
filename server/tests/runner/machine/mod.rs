//! Prepare an admitted QEMU machine and its run-specific boot material.
use crate::platform::{self, Machine, Purpose};
use crate::{Result, evidence, require};
use std::{env, path::Path, process::Command};

mod boot;

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
    boot::attach(&mut command, machine.platform.firmware, image, directory)?;
    transport(&mut command, machine, directory, observe);
    for device in &machine.devices {
        command.args(["-device", device]);
    }
    Ok(command)
}

fn transport(command: &mut Command, machine: &Machine, directory: &Path, observe: bool) {
    if machine.architecture == "x86_64" {
        command
            .arg("-debugcon")
            .arg(if observe {
                format!("file:{}", directory.join("debugcon.log").display())
            } else {
                "stdio".into()
            })
            .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=4"]);
    } else {
        // Semihosting diagnostics must not share the QMP control stream.
        let semihost = if observe {
            command.arg("-chardev").arg(format!(
                "file,id=semihost,path={}",
                directory.join("semihost.log").display()
            ));
            "enable=on,target=native,userspace=off,chardev=semihost"
        } else {
            "enable=on,target=native,userspace=off"
        };
        command.args(["-semihosting-config", semihost]);
        command.arg("-serial").arg(if observe {
            format!("file:{}", directory.join("serial.log").display())
        } else {
            "stdio".into()
        });
        if machine.secondary_serial {
            command.args(["-serial", "null"]);
        }
    }
}

/// Construct and record the selected machine without starting it.
pub fn prepare(
    image: &Path,
    purpose: Purpose,
    directory: &Path,
    observe: bool,
) -> Result<Command> {
    let machine = platform::read(image, purpose)?;
    evidence::json(
        &directory.join("platform.json"),
        &serde_json::json!({
            "architecture": machine.architecture,
            "purpose": purpose.name(),
            "firmware": format!("{:?}", machine.platform.firmware),
            "interrupt": format!("{:?}", machine.platform.interrupt),
            "harts": machine.platform.harts,
            "machine": machine.board,
            "cpu": machine.cpu,
            "iommu": format!("{:?}", machine.platform.iommu),
            "serial": format!("{:?}", machine.platform.serial),
            "devices": machine.devices,
            "image_sha256": evidence::digest(image)?,
        }),
    )?;
    let command = command(&machine, image, directory, observe)?;
    let version = output(Command::new(command.get_program()).arg("--version"))?;
    require(
        version
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(3))
            == Some(include_str!("../../qemu.version").trim()),
        "unexpected QEMU version",
    )?;
    evidence::json(
        &directory.join("tools.json"),
        &serde_json::json!({
            "qemu": version,
            "runner_sha256": evidence::digest(&env::current_exe()?)?,
        }),
    )?;
    Ok(command)
}

fn output(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    require(
        result.status.success(),
        format!("{command:?}: {}", String::from_utf8_lossy(&result.stderr)),
    )?;
    Ok(String::from_utf8(result.stdout)?.trim().into())
}

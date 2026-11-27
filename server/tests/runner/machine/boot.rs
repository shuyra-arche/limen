//! Resolve and attach the boot material selected by the admitted platform.
use super::variable;
use crate::platform::Firmware;
use crate::{Result, evidence, require};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy)]
enum Uefi {
    Aarch64,
    X86_64,
}

enum Material {
    Sbi(PathBuf),
    Uefi {
        architecture: Uefi,
        code: PathBuf,
        variables: PathBuf,
    },
}

impl Material {
    fn locate(firmware: Firmware) -> Self {
        let location = match firmware {
            Firmware::OpenSbi => variable(
                "QEMU_OPENSBI",
                "/usr/lib/riscv64-linux-gnu/opensbi/generic/fw_dynamic.bin",
            ),
            Firmware::Ovmf | Firmware::OvmfStrictNx => variable("OVMF_DIR", "/usr/share/OVMF"),
            Firmware::Aavmf | Firmware::AavmfStrictNx => variable("AAVMF_DIR", "/usr/share/AAVMF"),
        };
        Self::select(firmware, Path::new(&location))
    }

    fn select(firmware: Firmware, location: &Path) -> Self {
        let (architecture, code, variables) = match firmware {
            Firmware::OpenSbi => return Self::Sbi(location.to_path_buf()),
            Firmware::Ovmf => (Uefi::X86_64, "OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
            Firmware::Aavmf => (Uefi::Aarch64, "AAVMF_CODE.fd", "AAVMF_VARS.fd"),
            Firmware::OvmfStrictNx => (
                Uefi::X86_64,
                "OVMF_CODE_4M.secboot.strictnx.fd",
                "OVMF_VARS_4M.fd",
            ),
            Firmware::AavmfStrictNx => (
                Uefi::Aarch64,
                "AAVMF_CODE.secboot.strictnx.fd",
                "AAVMF_VARS.fd",
            ),
        };
        Self::Uefi {
            architecture,
            code: location.join(code),
            variables: location.join(variables),
        }
    }

    fn attach(&self, command: &mut Command, image: &Path, directory: &Path) -> Result<()> {
        match self {
            Self::Sbi(code) => {
                evidence::json(
                    &directory.join("firmware.json"),
                    &serde_json::json!({
                        "code": code,
                        "code_sha256": evidence::digest(code)?,
                    }),
                )?;
                command
                    .args(["-m", "128M", "-bios"])
                    .arg(code)
                    .arg("-kernel")
                    .arg(image);
                Ok(())
            }
            Self::Uefi {
                architecture,
                code,
                variables,
            } => architecture.attach(command, image, directory, code, variables),
        }
    }
}

impl Uefi {
    fn attach(
        self,
        command: &mut Command,
        image: &Path,
        directory: &Path,
        code: &Path,
        variables: &Path,
    ) -> Result<()> {
        require(
            code.is_file(),
            format!("missing firmware image: {}", code.display()),
        )?;
        let esp = directory.join("esp");
        let boot_dir = esp.join("EFI/BOOT");
        let boot = match self {
            Self::Aarch64 => "BOOTAA64.EFI",
            Self::X86_64 => "BOOTX64.EFI",
        };
        fs::create_dir_all(&boot_dir)?;
        fs::copy(image, boot_dir.join(boot))?;
        let private_vars = directory.join("vars.fd");
        fs::copy(variables, &private_vars)?;
        evidence::json(
            &directory.join("firmware.json"),
            &serde_json::json!({
                "code": code,
                "code_sha256": evidence::digest(code)?,
                "initial_vars_sha256": evidence::digest(&private_vars)?,
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
        match self {
            Self::X86_64 => {
                command
                    .args(["-m", "128M", "-drive"])
                    .arg(format!("format=raw,file=fat:rw:{}", esp.display()));
            }
            Self::Aarch64 => {
                command
                    .args(["-m", "256M", "-drive"])
                    .arg(format!(
                        "if=none,id=esp,format=raw,file=fat:rw:{}",
                        esp.display()
                    ))
                    .args(["-device", "virtio-blk-pci,drive=esp"]);
            }
        }
        Ok(())
    }
}

pub(super) fn attach(
    command: &mut Command,
    firmware: Firmware,
    image: &Path,
    directory: &Path,
) -> Result<()> {
    Material::locate(firmware).attach(command, image, directory)
}

#[cfg(test)]
#[path = "../tests/boot.rs"]
mod tests;

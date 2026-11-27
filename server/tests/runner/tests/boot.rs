use super::{Firmware, Material};
use crate::evidence;
use std::{fs, path::PathBuf, process::Command};

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("kernel-boot-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    root
}

#[test]
fn sbi_uses_the_selected_firmware_and_original_image() {
    let root = scratch("sbi");
    let code = root.join("sbi.bin");
    let image = root.join("kernel.elf");
    fs::write(&code, b"firmware").unwrap();
    fs::write(&image, b"guest").unwrap();
    let mut command = Command::new("unused");
    Material::select(Firmware::OpenSbi, &code)
        .attach(&mut command, &image, &root)
        .unwrap();
    let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();
    let bios = ["-bios", code.to_str().unwrap()];
    let kernel = ["-kernel", image.to_str().unwrap()];
    assert!(args.windows(2).any(|pair| pair == bios));
    assert!(args.windows(2).any(|pair| pair == kernel));
    assert!(root.join("firmware.json").is_file());
    assert!(!root.join("esp").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn uefi_strict_nx_requires_exact_code_and_private_variables() {
    let root = scratch("uefi");
    let image = root.join("kernel.efi");
    fs::write(&image, b"guest").unwrap();
    for (firmware, standard, strict, variables, boot) in [
        (
            Firmware::OvmfStrictNx,
            "OVMF_CODE_4M.fd",
            "OVMF_CODE_4M.secboot.strictnx.fd",
            "OVMF_VARS_4M.fd",
            "BOOTX64.EFI",
        ),
        (
            Firmware::AavmfStrictNx,
            "AAVMF_CODE.fd",
            "AAVMF_CODE.secboot.strictnx.fd",
            "AAVMF_VARS.fd",
            "BOOTAA64.EFI",
        ),
    ] {
        let directory = root.join(boot);
        fs::create_dir(&directory).unwrap();
        fs::write(root.join(standard), b"ordinary").unwrap();
        fs::write(root.join(variables), b"variables").unwrap();
        let material = Material::select(firmware, &root);
        let mut command = Command::new("unused");
        assert!(material.attach(&mut command, &image, &directory).is_err());
        assert!(!directory.join("firmware.json").exists());
        fs::write(root.join(strict), b"strict").unwrap();
        material.attach(&mut command, &image, &directory).unwrap();
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("firmware.json")).unwrap()).unwrap();
        let digest = evidence::digest(&root.join(strict)).unwrap();
        assert_eq!(receipt["code_sha256"].as_str(), Some(digest.as_str()));
        assert_eq!(
            fs::read(directory.join("esp/EFI/BOOT").join(boot)).unwrap(),
            b"guest"
        );
        fs::write(directory.join("vars.fd"), b"changed").unwrap();
        assert_eq!(fs::read(root.join(variables)).unwrap(), b"variables");
    }
    fs::remove_dir_all(root).unwrap();
}

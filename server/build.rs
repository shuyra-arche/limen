fn main() {
    println!("cargo::rustc-check-cfg=cfg(limen_target)");
    println!("cargo::rustc-check-cfg=cfg(limen_firmware, values(\"sbi\", \"uefi\"))");
    println!("cargo::rustc-check-cfg=cfg(limen_cooperative)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    // Describe the actual linked profiles, not every platform of an ISA.
    // Firmware entry and the availability of a cooperative switch are
    // independent build facts. Neither proves a running hardware capability.
    let (firmware, cooperative) = match (arch.as_str(), os.as_str()) {
        ("riscv64", "none") => ("sbi", true),
        ("x86_64" | "aarch64", "none" | "uefi") => ("uefi", false),
        _ => return,
    };
    println!("cargo::rustc-cfg=limen_target");
    println!("cargo::rustc-cfg=limen_firmware=\"{firmware}\"");
    if cooperative {
        println!("cargo::rustc-cfg=limen_cooperative");
    }
    if arch == "riscv64" {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("link.x");
        println!("cargo::rerun-if-changed=link.x");
        println!("cargo::metadata=linker_script={}", script.display());
    }
}

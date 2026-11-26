fn main() {
    println!("cargo::rustc-check-cfg=cfg(limen_target)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let supported = matches!(
        (arch.as_str(), os.as_str()),
        ("riscv64", "none") | ("x86_64" | "aarch64", "none" | "uefi")
    );
    if supported {
        println!("cargo::rustc-cfg=limen_target");
    }
    if !supported || arch != "riscv64" {
        return;
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("link.x");
    println!("cargo::rerun-if-changed=link.x");
    println!("cargo::metadata=linker_script={}", script.display());
}

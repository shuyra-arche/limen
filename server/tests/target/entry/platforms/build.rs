fn main() {
    assert!(std::env::var_os("DEP_SHUYRA_KERNEL_LINKER_SCRIPT").is_none());
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("virt.x");
        println!("cargo::rerun-if-changed=virt.x");
        println!("cargo::rustc-link-arg=-T{}", script.display());
    }
}

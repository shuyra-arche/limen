fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("riscv64") {
        return;
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("link.x");
    println!("cargo::rerun-if-changed=link.x");
    println!("cargo::metadata=linker_script={}", script.display());
}

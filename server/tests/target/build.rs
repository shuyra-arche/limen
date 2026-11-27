fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // The record must survive linker garbage collection in every profile.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("uefi") {
        println!("cargo::rustc-link-arg-tests=/include:LIMEN_TEST_PLATFORM");
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("riscv64") {
        return;
    }
    println!("cargo::rustc-link-arg-tests=--undefined=LIMEN_TEST_PLATFORM");
    let script = std::env::var("DEP_SHUYRA_KERNEL_LINKER_SCRIPT")
        .expect("Kernel provider must export its linker script");
    println!("cargo::rustc-link-arg-tests=--defsym=KERNEL_LOAD_ADDRESS=0x80200000");
    println!("cargo::rustc-link-arg-tests=--defsym=DTB_WINDOW_START=0x80200000");
    println!("cargo::rustc-link-arg-tests=--defsym=DTB_WINDOW_END=0x88000000");
    println!("cargo::rustc-link-arg-tests=-T{script}");
    println!("cargo::rerun-if-changed={script}");
}

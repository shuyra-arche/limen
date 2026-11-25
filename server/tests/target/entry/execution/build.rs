fn main() {
    let script = std::env::var("DEP_SHUYRA_KERNEL_LINKER_SCRIPT")
        .expect("Kernel provider must export its linker script");
    println!("cargo::rustc-link-arg=--defsym=KERNEL_LOAD_ADDRESS=0x80200000");
    println!("cargo::rustc-link-arg=--defsym=DTB_WINDOW_START=0x80200000");
    println!("cargo::rustc-link-arg=--defsym=DTB_WINDOW_END=0x88000000");
    println!("cargo::rustc-link-arg=-T{script}");
    println!("cargo::rerun-if-changed={script}");
}

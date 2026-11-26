#[path = "../../../metal/module/fixture.rs"]
mod fixture;

use std::{env, path::PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let probe = root.join("probe.x");
    let script = env::var("DEP_SHUYRA_KERNEL_LINKER_SCRIPT")
        .expect("Kernel provider must export its linker script");
    println!("cargo::rustc-link-arg=--defsym=KERNEL_LOAD_ADDRESS=0x80200000");
    println!("cargo::rustc-link-arg=--defsym=DTB_WINDOW_START=0x80200000");
    println!("cargo::rustc-link-arg=--defsym=DTB_WINDOW_END=0x88000000");
    println!("cargo::rustc-link-arg=-T{script}");
    println!("cargo::rustc-link-arg=-T{}", probe.display());
    println!("cargo::rerun-if-changed=probe.x");
    println!("cargo::rerun-if-changed={script}");
    println!("cargo::rerun-if-changed=../../../metal/module/fixture.rs");
    println!("cargo::rerun-if-changed=../../../metal/module/probe.rs");
    println!("cargo::rerun-if-changed=../../../metal/module/sv39_root.rs");

    const TARGET: &str = "riscv64gc-unknown-none-elf";
    let source = root.join("../../../metal/module");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fixture::build(&source.join("probe.rs"), TARGET, &output.join("module.elf"));
    fixture::build_module(
        &source.join("sv39_root.rs"),
        TARGET,
        &output.join("sv39-root.elf"),
        "sv39-root",
        "module_sv39_root",
        2,
    );
}

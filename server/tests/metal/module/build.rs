mod fixture;

use std::{env, path::PathBuf};

fn main() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../probe.rs");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=../probe.rs");
    println!("cargo:rerun-if-changed=../fixture.rs");
    println!("cargo:rerun-if-changed=../sv39_root.rs");

    for (target, file) in [
        ("riscv64gc-unknown-none-elf", "riscv64.elf"),
        ("x86_64-unknown-none", "x86_64.elf"),
        ("aarch64-unknown-none", "aarch64.elf"),
    ] {
        fixture::build(&source, target, &output.join(file));
    }
    fixture::build_module(
        &source.with_file_name("sv39_root.rs"),
        "riscv64gc-unknown-none-elf",
        &output.join("sv39-root.elf"),
        "sv39-root",
        "module_sv39_root",
        2,
    );
}

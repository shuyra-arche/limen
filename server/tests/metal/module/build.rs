mod fixture;

use std::{env, path::PathBuf};

fn main() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../probe.rs");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=../probe.rs");
    println!("cargo:rerun-if-changed=../fixture.rs");

    for (target, file) in [
        ("riscv64gc-unknown-none-elf", "riscv64.elf"),
        ("x86_64-unknown-none", "x86_64.elf"),
        ("aarch64-unknown-none", "aarch64.elf"),
    ] {
        fixture::build(&source, target, &output.join(file));
    }
}

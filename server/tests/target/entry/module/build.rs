#[path = "../../../metal/module/fixture.rs"]
mod fixture;

use std::{env, path::PathBuf};

fn main() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../metal/module/probe.rs");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("x86_64.elf");
    println!("cargo:rerun-if-changed=../../../metal/module/probe.rs");
    println!("cargo:rerun-if-changed=../../../metal/module/fixture.rs");
    fixture::build(&source, "x86_64-unknown-none", &output);
}

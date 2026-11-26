#[path = "../../../metal/module/fixture.rs"]
mod fixture;

use std::{env, path::PathBuf};

fn main() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../metal/module/probe.rs");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("module.elf");
    println!("cargo:rerun-if-changed=../../../metal/module/probe.rs");
    println!("cargo:rerun-if-changed=../../../metal/module/fixture.rs");
    let target = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86_64" => "x86_64-unknown-none",
        "aarch64" => "aarch64-unknown-none",
        _ => panic!("unsupported UEFI module target"),
    };
    fixture::build(&source, target, &output);
}

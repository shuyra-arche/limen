#[path = "../../metal/module/fixture.rs"]
mod fixture;
#[path = "../../metal/module/riscv_fixture.rs"]
mod riscv_fixture;

#[path = "../support/link.rs"]
mod link;

use std::{
    env,
    path::{Path, PathBuf},
};

fn module_images(architecture: &str) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../metal/module");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for name in ["fixture.rs", "probe.rs"] {
        println!("cargo::rerun-if-changed={}", source.join(name).display());
    }
    let target = match architecture {
        "riscv64" => "riscv64gc-unknown-none-elf",
        "aarch64" => "aarch64-unknown-none",
        "x86_64" => "x86_64-unknown-none",
        _ => panic!("unsupported module image architecture"),
    };
    fixture::build(&source.join("probe.rs"), target, &output.join("module.elf"));
    if architecture == "riscv64" {
        for name in ["riscv_fixture.rs", "riscv_root.rs"] {
            println!("cargo::rerun-if-changed={}", source.join(name).display());
        }
        riscv_fixture::build_roots(&source.join("riscv_root.rs"), target, &output);
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    link::configure(&root.join("../support"));

    let architecture = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if matches!(
        (architecture.as_str(), os.as_str()),
        ("riscv64", "none") | ("aarch64" | "x86_64", "uefi")
    ) {
        module_images(&architecture);
    }
}

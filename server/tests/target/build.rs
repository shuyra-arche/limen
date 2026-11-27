#[path = "../metal/module/fixture.rs"]
mod fixture;
#[path = "../metal/module/riscv_fixture.rs"]
mod riscv_fixture;

use std::{env, path::PathBuf};

fn module_images(architecture: &str) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../metal/module");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for name in ["fixture.rs", "probe.rs", "riscv_fixture.rs", "riscv_root.rs"] {
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
        riscv_fixture::build_roots(&source.join("riscv_root.rs"), target, &output);
    }
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // The record must survive linker garbage collection in every profile.
    let architecture = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("uefi") {
        println!("cargo::rustc-link-arg-tests=/include:LIMEN_TEST_PLATFORM");
        module_images(&architecture);
        return;
    }
    if architecture != "riscv64" || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
        return;
    }
    module_images(&architecture);
    println!("cargo::rustc-link-arg-tests=--undefined=LIMEN_TEST_PLATFORM");
    let script = env::var("DEP_SHUYRA_KERNEL_LINKER_SCRIPT")
        .expect("Kernel provider must export its linker script");
    println!("cargo::rustc-link-arg-tests=--defsym=KERNEL_LOAD_ADDRESS=0x80200000");
    println!("cargo::rustc-link-arg-tests=--defsym=DTB_WINDOW_START=0x80200000");
    println!("cargo::rustc-link-arg-tests=--defsym=DTB_WINDOW_END=0x88000000");
    println!("cargo::rustc-link-arg-tests=-T{script}");
    let extension = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("support/sbi/linker.ld");
    println!("cargo::rustc-link-arg-tests=-T{}", extension.display());
    println!("cargo::rustc-link-arg-tests=--entry=limen_test_image_entry");
    println!("cargo::rerun-if-changed={script}");
    println!("cargo::rerun-if-changed={}", extension.display());
}

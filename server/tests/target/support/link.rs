//! The common test-image linking contract, independent of test ownership.

use std::{env, path::Path};

pub fn configure(support: &Path) {
    println!("cargo::rerun-if-changed=build.rs");
    println!(
        "cargo::rerun-if-changed={}",
        support.join("link.rs").display()
    );
    // Tests and benches have the same image linking requirements.
    let architecture = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("uefi") {
        println!("cargo::rustc-link-arg=/include:LIMEN_TEST_PLATFORM");
        return;
    }
    if architecture != "riscv64" || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
        return;
    }
    println!("cargo::rustc-link-arg=--undefined=LIMEN_TEST_PLATFORM");
    let script = env::var("DEP_SHUYRA_KERNEL_LINKER_SCRIPT")
        .expect("Kernel provider must export its linker script");
    println!("cargo::rustc-link-arg=--defsym=KERNEL_LOAD_ADDRESS=0x80200000");
    println!("cargo::rustc-link-arg=--defsym=DTB_WINDOW_START=0x80200000");
    println!("cargo::rustc-link-arg=--defsym=DTB_WINDOW_END=0x88000000");
    println!("cargo::rustc-link-arg=-T{script}");
    let extension = support.join("sbi/linker.ld");
    println!("cargo::rustc-link-arg=-T{}", extension.display());
    println!("cargo::rustc-link-arg=--entry=limen_test_image_entry");
    // Without a probe, the script aliases the wrapped reference directly
    // to the original function. Retain that definition through section GC.
    println!("cargo::rustc-link-arg=--wrap=limen_switch_context");
    println!("cargo::rustc-link-arg=--undefined=__real_limen_switch_context");
    println!("cargo::rerun-if-changed={script}");
    println!("cargo::rerun-if-changed={}", extension.display());
}

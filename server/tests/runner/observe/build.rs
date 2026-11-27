use std::env;

fn main() {
    println!("cargo::rerun-if-env-changed=LIMEN_TEST_TARGET");
    println!(
        "cargo::rustc-check-cfg=cfg(limen_guest, values(\"riscv64\", \"aarch64\", \"x86_64\"))"
    );
    let architecture = match env::var("LIMEN_TEST_TARGET").as_deref() {
        Ok("riscv64gc-unknown-none-elf") => "riscv64",
        Ok("aarch64-unknown-uefi") => "aarch64",
        Ok("x86_64-unknown-uefi") => "x86_64",
        Err(env::VarError::NotPresent) => return,
        _ => panic!("unsupported guest target"),
    };
    println!("cargo::rustc-cfg=limen_guest=\"{architecture}\"");
}

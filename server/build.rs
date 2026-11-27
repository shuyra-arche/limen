use std::{env, path::PathBuf};

const RV64_LINKER: &str = "src/metal/platform/boot/riscv64/linker.ld";

enum Handoff {
    Sbi,
    Uefi,
}

enum Image {
    RiscvElf,
    PeCoff,
}

struct Profile {
    handoff: Handoff,
    image: Image,
    cooperative: bool,
}

fn profile(target: &str, arch: &str, os: &str) -> Option<Profile> {
    match (target, arch, os) {
        ("riscv64gc-unknown-none-elf", "riscv64", "none") => Some(Profile {
            handoff: Handoff::Sbi,
            image: Image::RiscvElf,
            cooperative: true,
        }),
        ("x86_64-unknown-uefi", "x86_64", "uefi") | ("aarch64-unknown-uefi", "aarch64", "uefi") => {
            Some(Profile {
                handoff: Handoff::Uefi,
                image: Image::PeCoff,
                cooperative: true,
            })
        }
        _ => None,
    }
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(limen_target)");
    println!("cargo::rustc-check-cfg=cfg(limen_firmware, values(\"sbi\", \"uefi\"))");
    println!("cargo::rustc-check-cfg=cfg(limen_cooperative)");

    let target = env::var("TARGET").expect("Cargo target");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture");
    let os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS");
    let Some(profile) = profile(&target, &arch, &os) else {
        return;
    };

    println!("cargo::rustc-cfg=limen_target");
    match profile.handoff {
        Handoff::Sbi => println!("cargo::rustc-cfg=limen_firmware=\"sbi\""),
        Handoff::Uefi => println!("cargo::rustc-cfg=limen_firmware=\"uefi\""),
    }
    if profile.cooperative {
        println!("cargo::rustc-cfg=limen_cooperative");
    }
    if let Image::RiscvElf = profile.image {
        let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("kernel manifest path"));
        let script = root.join(RV64_LINKER);
        println!("cargo::rerun-if-changed={RV64_LINKER}");
        println!("cargo::metadata=linker_script={}", script.display());
    }
}

#![no_std]

#[cfg(target_arch = "riscv64")]
mod basis;
#[cfg(any(
    target_arch = "riscv64",
    all(
        any(target_arch = "aarch64", target_arch = "x86_64"),
        any(target_os = "none", target_os = "uefi")
    ),
))]
pub mod entry;
#[cfg(any(
    target_arch = "riscv64",
    all(
        any(target_arch = "aarch64", target_arch = "x86_64"),
        any(target_os = "none", target_os = "uefi")
    ),
))]
mod metal;

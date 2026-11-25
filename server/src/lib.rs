#![no_std]
// Hardware fetches the RV64 early trap and Arm vector table at aligned addresses.
#![cfg_attr(
    any(
        target_arch = "riscv64",
        all(target_arch = "aarch64", any(target_os = "uefi", target_os = "none"))
    ),
    feature(fn_align)
)]

#[cfg(any(
    target_arch = "riscv64",
    all(
        any(target_arch = "aarch64", target_arch = "x86_64"),
        any(target_os = "none", target_os = "uefi")
    ),
))]
mod basis;
#[cfg(any(
    target_arch = "riscv64",
    all(
        any(target_arch = "aarch64", target_arch = "x86_64"),
        any(target_os = "none", target_os = "uefi")
    ),
))]
pub use basis::sync;
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

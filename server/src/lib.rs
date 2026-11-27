#![no_std]
// Hardware fetches the RV64 early trap and Arm vector table at aligned addresses.
#![cfg_attr(
    all(limen_target, any(target_arch = "riscv64", target_arch = "aarch64")),
    feature(fn_align)
)]

pub mod basis;
pub mod entry;
pub mod metal;

#[cfg(test)]
extern crate std;

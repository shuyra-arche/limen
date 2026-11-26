#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub mod acpi;
#[cfg(target_arch = "aarch64")]
#[cfg(limen_target)]
pub mod arm;
pub mod devicetree;
#[cfg(any(limen_target, test))]
pub(crate) mod facts;
pub mod map;
#[cfg(target_arch = "aarch64")]
#[cfg(limen_target)]
pub mod psci;
#[cfg(any(test, all(limen_target, target_arch = "riscv64")))]
pub mod sbi;
#[cfg(all(
    any(limen_target, test),
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
pub mod uefi;

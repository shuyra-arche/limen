#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(in crate::metal) mod native;

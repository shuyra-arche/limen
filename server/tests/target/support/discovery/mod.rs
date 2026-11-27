//! Default startup for a native test without extra firmware preparation.

mod harness;
pub use harness::{finish, run};

#[cfg(target_arch = "riscv64")]
kernel::limen_riscv_major!(stack: 32 * 1024, nodes: 5);

#[cfg(target_os = "uefi")]
#[uefi::entry]
fn main() -> uefi::Status {
    uefi_fixture::load(|| Ok(()))
}

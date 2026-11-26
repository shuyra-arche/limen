//! SBI firmware capability discovery and native calls.

mod probe;

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod transport;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use probe::SbiServices;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(crate) use transport::{
    admit, platform_services, services_on_entry, start_hart, trap_services,
};

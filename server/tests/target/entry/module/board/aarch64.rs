use core::arch::asm;
use kernel::metal::module::Machine;
use kernel::metal::{
    platform::description::devicetree::DeviceTree, platform::firmware::psci::Conduit, platform::firmware::psci::PsciDescription,
    platform::firmware::uefi::DeviceTree as LoaderTree,
    processor::hart::aarch64::description::Aarch64Cpus,
};
use uefi::Status;

#[path = "../../platforms/devicetree.rs"]
mod devicetree;

#[path = "../../platforms/pl011.rs"]
mod pl011;

pub const MACHINE: Machine = Machine::Aarch64;
pub const NAME: &str = "virt";

pub fn capture() -> Option<LoaderTree<'static>> {
    let bytes = devicetree::copy(|bytes| {
        let tree = DeviceTree::new(bytes).map_err(|_| Status::COMPROMISED_DATA)?;
        let cpus = Aarch64Cpus::read(&tree)
            .map_err(|_| Status::COMPROMISED_DATA)?
            .ok_or(Status::NOT_FOUND)?;
        let mut candidates = cpus.candidates();
        let cpu = candidates.next().ok_or(Status::NOT_FOUND)?;
        if !cpu.enabled || cpu.affinity != hart() || candidates.next().is_some() {
            return Err(Status::COMPROMISED_DATA);
        }
        let psci = PsciDescription::read(&tree)
            .map_err(|_| Status::COMPROMISED_DATA)?
            .ok_or(Status::NOT_FOUND)?;
        if psci.conduit() != Conduit::Hvc {
            return Err(Status::UNSUPPORTED);
        }
        unsafe {
            psci.verify().map_err(|_| Status::UNSUPPORTED)?;
            psci.verify_affinity(cpu.affinity, true)
                .map_err(|_| Status::DEVICE_ERROR)?;
        }
        Ok(())
    })
    .unwrap();
    Some(LoaderTree::Memory(bytes))
}

pub fn hart() -> usize {
    let affinity: usize;
    unsafe {
        asm!("mrs {}, MPIDR_EL1", out(reg) affinity, options(nomem, nostack, preserves_flags))
    };
    affinity & 0xff_00ff_ffff
}

pub fn write_byte(byte: u8) -> bool {
    // The owned QEMU virt profile supplies the identity-mapped PL011 window.
    unsafe { pl011::write_byte(0x0900_0000, byte) }
}

pub fn reset(_: bool) {
    // The profile runs firmware and the guest at EL1 with the HVC conduit.
    unsafe { Conduit::Hvc.system_off() };
}

/// The caller has validated the entry and published its executable image.
pub unsafe fn query(entry: usize) -> u64 {
    let query = unsafe { core::mem::transmute::<usize, unsafe extern "C" fn() -> u64>(entry) };
    unsafe { query() }
}

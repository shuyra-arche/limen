//! q35 legacy mask and local APIC setup shared by native interrupt guests.
use kernel::metal::memory::access::treaty::device::DeviceWindow;
use uefi_fixture::q35::io;

/// Prepare the exclusive local APIC window for an IRQ in priority class 15.
///
/// # Safety
/// The guest owns both legacy PICs and its original processor's UC xAPIC mapping.
pub unsafe fn prepare(window: &mut DeviceWindow<'_>) {
    unsafe {
        io::write8(0x21, 0xff);
        io::write8(0xa1, 0xff);
        window.write_u32(0x80, 0xe0).unwrap();
        window.write_u32(0xf0, 0x1ff).unwrap();
    }
}

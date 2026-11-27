//! Checked device geometry never turns the address into ordinary RAM.
use kernel::metal::memory::mmu::access::device::{DeviceWindow, Error};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};

#[test]
fn device_window_rejects_invalid_geometry_before_access() {
    let physical = PhysicalAddress::new(0x1000)
        .range(ByteLength::new(16))
        .unwrap();
    assert_eq!(
        unsafe { DeviceWindow::admit(physical, InternalAddress::new(0)) }.err(),
        Some(Error::Range)
    );
    assert_eq!(
        unsafe { DeviceWindow::admit(physical, InternalAddress::new(usize::MAX as u64 - 7)) }.err(),
        Some(Error::Range)
    );
}

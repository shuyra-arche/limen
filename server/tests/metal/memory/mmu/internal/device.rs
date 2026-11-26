//! Checked device geometry never turns the address into ordinary RAM.
use kernel::metal::memory::mmu::internal::device::{DeviceWindow, Error};
use limen_treaty::model::{ByteLength, PhysicalAddress};

#[test]
fn device_window_rejects_out_of_range_word_geometry() {
    let physical = PhysicalAddress::new(0x1000).range(ByteLength::new(16)).unwrap();
    // Geometry only: never dereference these addresses.
    let window = unsafe { DeviceWindow::admit(physical, 0x2000) }.unwrap();
    assert_eq!(window.physical(), physical);
    assert_eq!(window.word(0), Some(0x2000));
    assert_eq!(window.word(12), Some(0x200c));
    assert_eq!(window.word(13), None);
    assert_eq!(window.word(16), None);
    assert_eq!(window.word(usize::MAX), None);
    assert_eq!(unsafe { DeviceWindow::admit(physical, 0) }.err(), Some(Error::Range));
    assert_eq!(unsafe { DeviceWindow::admit(physical, usize::MAX - 7) }.err(), Some(Error::Range));
}

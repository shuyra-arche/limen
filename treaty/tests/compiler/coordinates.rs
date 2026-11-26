#![no_std]

use limen_treaty::model::{ByteLength, InternalAddress, PageCount, PageOffset, PhysicalAddress};

pub fn lengths(address: InternalAddress, physical: PhysicalAddress) -> (ByteLength, PageCount) {
    let bytes = address.range(ByteLength::new(1)).unwrap().length();
    let _ = physical.range(bytes).unwrap();
    let pages = PageOffset::new(0).range(PageCount::new(1)).unwrap().length();
    (bytes, pages)
}

#[cfg(contract = "compare")]
pub fn compare(address: InternalAddress, physical: PhysicalAddress) -> bool {
    address == physical
}

#[cfg(contract = "substitute")]
pub fn substitute(physical: PhysicalAddress) -> InternalAddress {
    physical.advance(ByteLength::new(1)).unwrap()
}

#[cfg(contract = "measure")]
pub fn measure(address: InternalAddress) {
    let _ = address.advance(PageCount::new(1));
}

#[cfg(contract = "range")]
pub fn range(address: InternalAddress, physical: PhysicalAddress) {
    let _ = address.range(ByteLength::new(1)).unwrap().contains(physical);
}

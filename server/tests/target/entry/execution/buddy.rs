use core::mem::MaybeUninit;
use kernel::{
    address::{PhysicalAddress, PhysicalRange},
    allocator::buddy::{Buddy, Error, Geometry, Root},
};
use limen_treaty::model::ByteLength;

const ORDER: u32 = 14;
const GRAINS: u64 = 1 << ORDER;
const NODES: usize = 2 * GRAINS as usize - 1;
static mut STORAGE: [MaybeUninit<u8>; NODES] = [MaybeUninit::uninit(); NODES];

fn physical(start: u64, end: u64) -> PhysicalRange {
    PhysicalRange::new(PhysicalAddress::new(start), PhysicalAddress::new(end)).unwrap()
}

pub(super) fn check() {
    let geometry = Geometry::new(physical(0, GRAINS), ByteLength::new(1)).unwrap();
    let bound = geometry.edit_bound();
    let mut roots = [MaybeUninit::<Root>::uninit()];
    // The first Unit runs this once and the fixture has no other users.
    let nodes = unsafe { &mut *(&raw mut STORAGE) };
    let mut buddy = Buddy::new(geometry, &mut roots, nodes, bound).unwrap();
    let last = GRAINS - 1;
    assert!(buddy.occupy(physical(last, GRAINS)).unwrap() <= bound);
    assert_eq!(buddy.largest_free_order(), Some(ORDER - 1));
    let revision = buddy.revision();
    assert_eq!(buddy.occupy(physical(0, GRAINS)), Err(Error::State));
    assert_eq!(buddy.revision(), revision);
    assert!(buddy.occupy(physical(0, 1)).unwrap() <= bound);
    assert!(buddy.release(physical(last, GRAINS)).unwrap() <= bound);
    assert!(buddy.release(physical(0, 1)).unwrap() <= bound);
    assert_eq!(buddy.largest_free_order(), Some(ORDER));
}

use core::mem::MaybeUninit;
use kernel::allocator::buddy::{Buddy, Error, Geometry, Root};

const ORDER: u32 = 14;
const GRAINS: u64 = 1 << ORDER;
const NODES: usize = 2 * GRAINS as usize - 1;
static mut STORAGE: [MaybeUninit<u8>; NODES] = [MaybeUninit::uninit(); NODES];

pub(super) fn check() {
    let geometry = Geometry::new(0..GRAINS, 1).unwrap();
    let bound = geometry.edit_bound();
    let mut roots = [MaybeUninit::<Root>::uninit()];
    // The first Unit runs this once and the fixture has no other users.
    let nodes = unsafe { &mut *(&raw mut STORAGE) };
    let mut buddy = Buddy::new(geometry, &mut roots, nodes, bound).unwrap();
    let last = GRAINS - 1;
    assert!(buddy.occupy(last..GRAINS).unwrap() <= bound);
    assert_eq!(buddy.largest_free_order(), Some(ORDER - 1));
    let revision = buddy.revision();
    assert_eq!(buddy.occupy(0..GRAINS), Err(Error::State));
    assert_eq!(buddy.revision(), revision);
    assert!(buddy.occupy(0..1).unwrap() <= bound);
    assert!(buddy.release(last..GRAINS).unwrap() <= bound);
    assert!(buddy.release(0..1).unwrap() <= bound);
    assert_eq!(buddy.largest_free_order(), Some(ORDER));
}

use core::{
    hint::black_box,
    mem::{MaybeUninit, align_of, size_of},
    ptr,
};

use kernel::{
    entry::boot::{Boot, write},
    metal::allocator::{
        Allocation, Allowance,
        pool::{GroupSlot, Pool, Prepared, Reservation, VALUE_ALIGNMENT, VALUE_BYTES},
    },
};

use super::pool_material::{group_extent, install};

#[repr(align(64))]
struct Boundary([u8; VALUE_BYTES]);

static mut GROUPS: [GroupSlot<'static, Boundary>; 1] = [GroupSlot::EMPTY];
static mut TYPED: MaybeUninit<Pool<'static, Boundary>> = MaybeUninit::uninit();
static mut ALLOCATION: Option<Allocation> = None;
static mut ALLOWANCE: Option<Allowance> = None;

pub(super) fn prepare(boot: &Boot<'_>) {
    assert_eq!(size_of::<Boundary>(), VALUE_BYTES);
    assert_eq!(align_of::<Boundary>(), VALUE_ALIGNMENT);
    let directory = unsafe { &mut *(&raw mut GROUPS) };
    let allocation = unsafe { &mut *(&raw mut ALLOCATION) };
    let allowance = unsafe { &mut *(&raw mut ALLOWANCE) };
    let (pool, _, _) = unsafe { install(boot, directory, allocation, allowance) };
    unsafe { (&mut *(&raw mut TYPED)).write(pool) };
}

// Keep the boundary case visible as a distinct target call frame.
#[inline(never)]
fn initialize(
    slot: Reservation<'static, 'static, Boundary>,
    byte: u8,
) -> Prepared<'static, 'static, Boundary> {
    slot.initialize(black_box(Boundary([byte; VALUE_BYTES])))
}

// Runs on the first actual Kernel Unit's 16 KiB stack.
#[inline(never)]
pub(super) fn check() {
    let pool = unsafe { (&*(&raw const TYPED)).assume_init_ref() };
    let mut reservation = None;
    pool.reserve(&mut reservation).unwrap();
    let prepared = initialize(reservation.unwrap(), black_box(0x65));
    let mut owner = None;
    prepared.publish(&mut owner).ok().unwrap();
    let object = owner.unwrap();
    let value = object.get();
    let address = ptr::from_ref(value).addr();
    assert!(address.is_multiple_of(VALUE_ALIGNMENT));
    let storage = super::memory::extent(address as u64, (address + VALUE_BYTES) as u64);
    assert!(group_extent().covers(storage));
    assert!(value.0.iter().all(|&byte| byte == 0x65));
    object.destroy().ok().unwrap().release().ok().unwrap();
    write("PASS pool value-bytes=512 value-align=64 unit-stack=1\n");
}

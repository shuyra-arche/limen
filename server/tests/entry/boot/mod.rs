#[path = "../../../src/metal/mmu/internal/access.rs"]
pub mod access;
#[path = "../../../src/metal/firmware/devicetree/mod.rs"]
mod devicetree;
mod memory;

#[repr(align(8))]
struct Header([u32; 10]);

fn admitted(structure_size: u32, strings: u32, strings_size: u32, reservations: u32) -> bool {
    let words = [
        0xd00d_feed,
        128,
        56,
        strings,
        reservations,
        17,
        16,
        0,
        strings_size,
        structure_size,
    ];
    let header = Header(words.map(u32::to_be));
    // The parser reads only this aligned, initialized header.
    unsafe { devicetree::extent(&raw const header.0, 0..0).is_some() }
}

#[test]
fn admits_adjacent_blocks_and_an_empty_strings_block() {
    assert!(admitted(16, 72, 16, 40));
    assert!(admitted(16, 72, 0, 40));
}

#[test]
fn rejects_impossible_structure_lengths_and_overlapping_blocks() {
    assert!(!admitted(0, 72, 16, 40));
    assert!(!admitted(12, 72, 16, 40));
    assert!(!admitted(17, 80, 8, 40));
    assert!(!admitted(32, 72, 8, 40));
    assert!(!admitted(16, 80, 8, 48));
    assert!(!admitted(16, 48, 8, 40));
}

#[path = "../../../src/metal/firmware/uefi/loader.rs"]
mod loader;
use handoff::Boot;
#[path = "../../../src/metal/firmware/map.rs"]
mod map;
use devicetree::memory::{DtbMemory, Physical, Source};
use loader::{DeviceTree, Loader};
use map::{FirmwareMap, FirmwareRegion, MapError};
use physical::{Exclusions, Retirement};
#[path = "../../../src/metal/allocator/bump.rs"]
mod bump;
mod exclusions;
#[path = "../../../src/entry/boot/handoff.rs"]
mod handoff;
mod organization;
#[path = "../../../src/entry/boot/physical.rs"]
mod physical;
#[path = "../../../src/entry/boot/regions.rs"]
mod regions;

const IMAGE: Physical = Physical {
    start: 0x1000,
    end: 0x2000,
};

#[test]
fn admits_only_the_reserved_dtb_extent_from_a_strided_final_map() {
    #[repr(align(8))]
    struct Bytes<const N: usize>([u8; N]);
    let mut dtb = Bytes([0; 96]);
    let words = [0xd00d_feed_u32, 96, 56, 72, 40, 17, 16, 0, 0, 16];
    for (dst, word) in dtb.0.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        dst.copy_from_slice(&word.to_be_bytes());
    }
    let mut descriptors = Bytes([0u8; 96]);
    // A real reserved descriptor, not a zero-length placeholder.
    descriptors.0[24..32].copy_from_slice(&1u64.to_le_bytes());
    let d = &mut descriptors.0[48..];
    d[..4].copy_from_slice(&2u32.to_le_bytes());
    d[8..16].copy_from_slice(&((dtb.0.as_ptr().addr() as u64) & !4095).to_le_bytes());
    d[24..32].copy_from_slice(&2u64.to_le_bytes());
    let admit = |descriptors: &[u8], stride, version, tree: Option<&[u8]>| {
        let loader = Loader {
            image: IMAGE,
            descriptors,
            stride,
            version,
            descriptor_limit: 2,
            device_tree: tree.map(DeviceTree::Description),
        };
        loader.admit(7).map(|b| {
            assert_eq!(b.hart(), 7);
            assert_eq!(b.firmware_map().unwrap().stride(), stride);
            assert!(b.dtb_memory().is_none());
            assert!(b.conventional_ram().is_none());
            b.device_tree()
        })
    };
    assert_eq!(
        admit(&descriptors.0, 48, 1, Some(&dtb.0)),
        Some(Some(dtb.0.as_ptr().addr()..dtb.0.as_ptr().addr() + 96))
    );
    assert_eq!(admit(&descriptors.0, 48, 1, None), Some(None));
    let loader = Loader {
        image: IMAGE,
        descriptors: &descriptors.0,
        stride: 48,
        version: 1,
        descriptor_limit: 2,
        device_tree: Some(DeviceTree::Memory(&dtb.0)),
    };
    assert!(loader.admit(7).is_none());
    assert!(admit(&descriptors.0, 48, 1, Some(&dtb.0[..40])).is_none());
    assert!(admit(&descriptors.0, 0, 1, None).is_none());
    assert!(admit(&descriptors.0, 48, 2, None).is_none());
    descriptors.0[48] = 4;
    assert!(admit(&descriptors.0, 48, 1, Some(&dtb.0)).is_none());
}

#[repr(align(8))]
struct DescriptorBytes([u8; 96]);

fn describe(entry: &mut [u8], kind: u32, base: u64, pages: u64, attributes: u64) {
    entry[..4].copy_from_slice(&kind.to_le_bytes());
    entry[8..16].copy_from_slice(&base.to_le_bytes());
    entry[24..32].copy_from_slice(&pages.to_le_bytes());
    entry[32..40].copy_from_slice(&attributes.to_le_bytes());
}

#[test]
fn firmware_map_preserves_unsorted_adjacent_regions_and_raw_attributes() {
    let mut bytes = DescriptorBytes([0xff; 96]);
    describe(&mut bytes.0[..48], 7, 0x3000, 2, 8);
    describe(&mut bytes.0[48..], 0, 0x1000, 2, 0x8000_0000_0000_0000);
    let map = FirmwareMap::new(&bytes.0, 48, 1, 2).unwrap();
    assert_eq!(map.descriptors(), &bytes.0);
    assert_eq!(map.stride(), 48);
    assert_eq!(map.version(), 1);
    let mut regions = map.regions();
    assert_eq!(regions.len(), 2);
    let first = regions.next().unwrap();
    assert_eq!(first.physical, 0x3000..0x5000);
    assert_eq!(first.kind, 7);
    assert_eq!(first.attributes, 8);
    let second = regions.next().unwrap();
    assert_eq!(second.physical, 0x1000..0x3000);
    assert_eq!(second.kind, 0);
    assert_eq!(second.attributes, 0x8000_0000_0000_0000);
    assert!(regions.next().is_none());
}

#[test]
fn firmware_map_rejects_bad_geometry_overlaps_and_capacity_before_handoff() {
    let mut bytes = DescriptorBytes([0; 96]);
    describe(&mut bytes.0[..48], 7, 0x1000, 1, 0);
    describe(&mut bytes.0[48..], 2, 0x2000, 1, 0);
    assert_eq!(
        FirmwareMap::new(&bytes.0, 48, 1, 1).err(),
        Some(MapError::Capacity)
    );
    assert_eq!(
        FirmwareMap::new(&bytes.0, 40, 1, 2).err(),
        Some(MapError::Format)
    );
    assert_eq!(
        FirmwareMap::new(&bytes.0, 48, 2, 2).err(),
        Some(MapError::Format)
    );
    assert_eq!(
        FirmwareMap::new(&bytes.0[1..49], 48, 1, 2).err(),
        Some(MapError::Format)
    );
    for (base, pages, error) in [
        (0x1000, 1, MapError::Overlap),
        (0x1001, 1, MapError::Range),
        (0x2000, 0, MapError::Range),
        (0x2000, u64::MAX, MapError::Range),
        (u64::MAX - 4095, 1, MapError::Range),
    ] {
        describe(&mut bytes.0[48..], 2, base, pages, 0);
        assert_eq!(FirmwareMap::new(&bytes.0, 48, 1, 2).err(), Some(error));
        let loader = Loader {
            image: IMAGE,
            descriptors: &bytes.0,
            stride: 48,
            version: 1,
            descriptor_limit: 2,
            device_tree: None,
        };
        assert!(loader.admit(7).is_none());
    }
}

#[path = "../../../src/metal/boot/console.rs"]
mod console;

#[test]
fn console_stops_at_the_first_byte_the_output_cannot_accept() {
    use core::fmt::Write;
    use core::sync::atomic::{AtomicUsize, Ordering};

    static WRITES: AtomicUsize = AtomicUsize::new(0);
    let mut writer = console::Console {
        write_byte: |byte| {
            let index = WRITES.fetch_add(1, Ordering::Relaxed);
            assert_eq!(byte, "圜".as_bytes()[index]);
            index == 0
        },
    };
    assert!(writer.write_str("圜").is_err());
    assert_eq!(WRITES.load(Ordering::Relaxed), 2);
}

// Compile the same source and Turn queue against a host-only Hart prefix.
#[allow(dead_code)]
#[path = "../../../src/metal/allocator/mod.rs"]
pub mod allocator;
#[allow(dead_code)]
#[path = "../../../src/metal/identity/mod.rs"]
mod identity;
#[path = "../../../src/basis/pend.rs"]
pub mod pend;
mod pool;
#[path = "../../../src/basis/sync/queue.rs"]
mod queue;
mod source;
#[allow(dead_code)]
#[path = "../../../src/basis/sync/turn.rs"]
mod turn;

mod entry {
    pub mod boot {
        pub use crate::{
            devicetree::memory::Physical,
            physical::Retirement,
            regions::{Region, RegionError, RegionSource, Regions, RootStorage},
        };
    }
}

mod sync {
    pub use crate::turn::*;
}

mod metal {
    pub(crate) use crate::identity;

    pub mod hart {
        include!("../../../src/metal/hart/node.rs");

        use std::cell::Cell;

        thread_local! { static ACTIVE: Cell<bool> = const { Cell::new(false) }; }

        pub(crate) enum Error {
            Capacity,
            Context,
        }

        pub(crate) struct TurnParticipation(Box<[QueueNode; crate::allocator::REGION_LIMIT + 2]>);

        pub(crate) fn begin_turn(count: usize) -> Result<TurnParticipation, Error> {
            if count > crate::allocator::REGION_LIMIT + 2 {
                return Err(Error::Capacity);
            }
            if ACTIVE.with(|active| active.replace(true)) {
                return Err(Error::Context);
            }
            Ok(TurnParticipation(Box::new(core::array::from_fn(|_| {
                QueueNode::new()
            }))))
        }

        impl TurnParticipation {
            pub(crate) fn node(&self, index: usize) -> &QueueNode {
                &self.0[index]
            }

            pub(crate) fn held(&self) {}

            pub(crate) fn releasing(&self) {}
        }

        impl Drop for TurnParticipation {
            fn drop(&mut self) {
                ACTIVE.with(|active| assert!(active.replace(false)));
            }
        }
    }
}

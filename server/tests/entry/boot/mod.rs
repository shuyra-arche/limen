#[path = "../../../src/metal/generic/devicetree.rs"]
mod devicetree;

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

#[path = "../../../src/metal/generic/firmware/uefi/loader.rs"]
mod loader;
use handoff::Boot;
#[path = "../../../src/metal/generic/firmware/map.rs"]
mod map;
use loader::Loader;
use map::FirmwareMap;
#[path = "../../../src/entry/boot/handoff.rs"]
mod handoff;

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
    let d = &mut descriptors.0[48..];
    d[..4].copy_from_slice(&2u32.to_le_bytes());
    d[8..16].copy_from_slice(&((dtb.0.as_ptr().addr() as u64) & !4095).to_le_bytes());
    d[24..32].copy_from_slice(&2u64.to_le_bytes());
    let admit = |descriptors: &[u8], stride, version, tree: Option<&[u8]>| {
        let loader = Loader {
            map: FirmwareMap {
                descriptors,
                stride,
                version,
            },
            device_tree: tree,
        };
        loader.admit(7).map(|b| {
            assert_eq!(b.hart(), 7);
            assert_eq!(b.firmware_map().unwrap().stride, stride);
            b.device_tree()
        })
    };
    assert_eq!(
        admit(&descriptors.0, 48, 1, Some(&dtb.0)),
        Some(Some(dtb.0.as_ptr().addr()..dtb.0.as_ptr().addr() + 96))
    );
    assert_eq!(admit(&descriptors.0, 48, 1, None), Some(None));
    assert!(admit(&descriptors.0, 48, 1, Some(&dtb.0[..40])).is_none());
    assert!(admit(&descriptors.0, 0, 1, None).is_none());
    assert!(admit(&descriptors.0, 48, 2, None).is_none());
    descriptors.0[48] = 4;
    assert!(admit(&descriptors.0, 48, 1, Some(&dtb.0)).is_none());
}

#[path = "../../../src/metal/generic/console.rs"]
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

//! Own, activate, and retire the first complete EL1 upper table tree.

use core::{arch::asm, mem::MaybeUninit, ptr};

use kernel::{
    basis::nest::{MappingStorage, Nest, Progress, SpanStorage},
    entry::boot::{Boot, Retirement},
    metal::{
        memory::mmu::{
            aarch64::internal::{context::Controls, installation::admit_inactive_upper},
            treaty::{LeafAccess, Memory, TableFormat},
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress, PhysicalRange};

use super::super::memory::{self, extent};

#[path = "../internal/mapping.rs"]
mod mapping;
#[path = "../internal/owned.rs"]
mod tree;
use tree::Tree;
pub(super) const PAGES: usize = tree::PAGES;

const PAGE: u64 = 4096;
const UPPER: u64 = 0xffff_0000_0000_0000;
const OUTPUT: u64 = 0x0000_ffff_ffff_f000;
const MEMORY_ATTRIBUTES: u64 = 0xff00_0000_0000_0180;

#[repr(C, align(4096))]
struct Witness([u64; 512]);

static WITNESS: Witness = Witness([0x50f1_0ace_d000_4321; 512]);
static REPLACEMENT: Witness = Witness([0x8c11_772e_3210_6543; 512]);
static mut NEST: MaybeUninit<Nest> = MaybeUninit::uninit();
static mut SPANS: SpanStorage<1> = SpanStorage::new();
static mut MAPPINGS: MappingStorage<1> = MappingStorage::new();

fn translation(address: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!("at s1e1r, {}", "isb", "mrs {}, par_el1",
            in(reg) address, out(reg) result, options(nostack));
    }
    result
}

fn write_translation(address: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!("at s1e1w, {}", "isb", "mrs {}, par_el1",
            in(reg) address, out(reg) result, options(nostack));
    }
    result
}

fn verify_lower_writable(range: PhysicalRange) {
    let start = range.start().get();
    let end = range.end().get();
    for page in (start..end).step_by(PAGE as usize) {
        let first = write_translation(page);
        let last = write_translation(page + PAGE - 1);
        assert_eq!(first & (OUTPUT | 1), page);
        assert_eq!(last & (OUTPUT | 1), page);
        assert_eq!(first & MEMORY_ATTRIBUTES, last & MEMORY_ATTRIBUTES);
    }
}

/// # Safety
/// The fixture owns the original unexposed grant and its writable lower
/// mapping. The lower walk remains resident and is the only live walk.
pub(super) unsafe fn check(boot: &Boot<'_>, range: PhysicalRange, evidence: &Retirement) {
    verify_lower_writable(range);
    let original = unsafe { Controls::current() };
    let (mut control, profile) = unsafe { admit_inactive_upper(boot.processor()) }.unwrap();
    let mut tree =
        unsafe { Tree::prepare(boot, range, evidence, TableFormat::Aarch64(profile), 3) };
    let [leaf, _, _, root] = tree.nodes;

    let lower = ptr::from_ref(&WITNESS).addr() as u64;
    let original_witness = translation(lower);
    assert_eq!(original_witness & 1, 0);
    let witness = original_witness & OUTPUT;
    assert!(boot.image().covers(extent(witness, witness + PAGE)));
    assert_ne!(translation(UPPER) & 1, 0);
    unsafe {
        leaf.store.map_leaf(
            leaf.id,
            0,
            PhysicalAddress::new(witness),
            LeafAccess::ReadOnly,
            Memory::Normal,
            Fast,
        )
    }
    .unwrap();
    tree.link([0, 0, 0]);
    let charged = tree.allowance.balance().unwrap();

    let root_address = root.store.cell(root.id).unwrap().1.start().get();
    let mut active = unsafe { root.store.prepare_local(root.id, &mut control, Fast) }.unwrap();
    let mut retired = None;
    assert!(
        unsafe { root.store.retire_private(root.id, &mut retired, Fast) }.is_err()
            && retired.is_none()
    );
    unsafe { active.install() }.unwrap();
    let current = unsafe { Controls::current() };
    assert_eq!(current.ttbr1, root_address);
    assert_eq!(current.ttbr0, original.ttbr0);
    const UPPER_FIELDS: u64 = 0xffbf << 16;
    assert_eq!(current.tcr & !UPPER_FIELDS, original.tcr & !UPPER_FIELDS);
    assert_eq!(current.mair, original.mair);
    assert_eq!(current.sctlr, original.sctlr);
    assert_eq!(translation(lower), original_witness);
    let on_upper = translation(UPPER);
    assert_eq!(on_upper & (OUTPUT | 1), witness);
    assert_eq!(
        on_upper & MEMORY_ATTRIBUTES,
        original_witness & MEMORY_ATTRIBUTES
    );
    assert_ne!(write_translation(UPPER) & 1, 0);
    let value: u64;
    // Raw register load avoids fabricating a second Rust reference. The
    // descriptor and actual native AT prove the original, immutable page.
    unsafe { asm!("ldr {}, [{}]", out(reg) value, in(reg) UPPER, options(nostack, readonly)) };
    assert_eq!(value, WITNESS.0[0]);

    // Retain the actual live leaf slot, not a second private byte borrow.
    use kernel::metal::memory::mmu::treaty::{SwitchError, TranslationPhase};
    let address = InternalAddress::new(UPPER + PAGE);
    let target = PhysicalAddress::new(witness);
    let mut edit = active
        .prepare_translation(address, target, ByteLength::new(PAGE), LeafAccess::ReadOnly, Fast)
        .unwrap();
    assert_eq!(edit.phase(), TranslationPhase::Prepared);
    assert!(matches!(
        active.prepare_translation(address, target, ByteLength::new(PAGE), LeafAccess::ReadOnly, Fast),
        Err(kernel::metal::memory::mmu::treaty::TableError::Occupied)
    ));
    assert_eq!(unsafe { active.restore() }, Err(SwitchError::Current));
    unsafe { active.install_translation(&mut edit, Fast) }.unwrap();
    assert_eq!(edit.phase(), TranslationPhase::Active);
    let value: u64;
    unsafe {
        asm!("ldr {}, [{}]", out(reg) value, in(reg) address.get(), options(nostack, readonly))
    };
    assert_eq!(value, WITNESS.0[0]);
    unsafe { active.remove_translation(&mut edit, Fast) }.unwrap();
    assert_eq!(edit.phase(), TranslationPhase::Removed);
    assert_ne!(translation(address.get()) & 1, 0);
    let lower_replacement = ptr::from_ref(&REPLACEMENT).addr() as u64;
    let replacement_translation = translation(lower_replacement);
    assert_eq!(replacement_translation & 1, 0);
    let replacement_physical = replacement_translation & OUTPUT;
    assert!(
        boot.image()
            .covers(extent(replacement_physical, replacement_physical + PAGE))
    );
    assert_ne!(replacement_physical, witness);
    let mut replacement = active
        .prepare_translation(
            address,
            PhysicalAddress::new(replacement_physical),
            ByteLength::new(PAGE),
            LeafAccess::ReadOnly,
            Fast,
        )
        .unwrap();
    // Repeated retirement of the original cannot cancel the replacement.
    unsafe { active.remove_translation(&mut edit, Fast) }.unwrap();
    unsafe { active.install_translation(&mut replacement, Fast) }.unwrap();
    let value: u64;
    unsafe {
        asm!("ldr {}, [{}]", out(reg) value, in(reg) address.get(), options(nostack, readonly))
    };
    assert_eq!(value, REPLACEMENT.0[0]);
    unsafe { active.remove_translation(&mut replacement, Fast) }.unwrap();
    let mut cancelled = active
        .prepare_translation(address, target, ByteLength::new(PAGE), LeafAccess::ReadOnly, Fast)
        .unwrap();
    unsafe { active.remove_translation(&mut cancelled, Fast) }.unwrap();
    assert_eq!(cancelled.phase(), TranslationPhase::Removed);

    assert!(root.store.unlink_child(root.id, 0, Fast).is_err());
    assert!(active.finish(Fast).is_err());
    assert_eq!(tree.allowance.balance(), Ok(charged));

    let nest = unsafe {
        (&mut *(&raw mut NEST))
            .write(Nest::new(&mut *(&raw mut SPANS), &mut *(&raw mut MAPPINGS)).unwrap())
    };
    let mut owner = nest.publish(active).ok().unwrap();
    let spare_start = range.start().get() + 6 * PAGE;
    let data_start = (spare_start + PAGE).next_multiple_of(2 * PAGE);
    let data = extent(data_start, data_start + 2 * PAGE);
    assert!(range.covers(data));
    let spare = unsafe {
        tree.prepare_leaf(
            TableFormat::Aarch64(profile),
            extent(spare_start, spare_start + PAGE),
        )
    };
    let expanded = tree.allowance.balance().unwrap();
    assert_eq!(expanded.occupied().get(), charged.occupied().get() + PAGE);
    let dynamic_address = UPPER + (1 << 21);
    unsafe {
        mapping::check(
            &mut owner,
            tree.allowance,
            data,
            dynamic_address,
            &[(spare.store, spare.id)],
            memory::mapped,
            |address| {
                let physical = data_start + (address - dynamic_address);
                let observed = translation(address);
                assert_eq!(observed & (OUTPUT | 1), physical);
                assert_eq!(
                    observed & MEMORY_ATTRIBUTES,
                    translation(physical) & MEMORY_ATTRIBUTES
                );
                let value: u64;
                asm!("ldr {}, [{}]", out(reg) value, in(reg) address, options(nostack, readonly));
                value
            },
            |address, value| {
                let physical = data_start + (address - dynamic_address);
                assert_eq!(write_translation(address) & (OUTPUT | 1), physical);
                asm!("str {}, [{}]", in(reg) value, in(reg) address, options(nostack));
            },
        )
    };
    assert_ne!(translation(dynamic_address) & 1, 0);
    assert_ne!(translation(dynamic_address + PAGE) & 1, 0);
    assert_eq!(tree.allowance.balance(), Ok(expanded));
    owner.begin_delete().unwrap();
    owner.begin_drain().unwrap();
    assert_eq!(unsafe { owner.advance_retirement() }, Ok(Progress::Pending));
    assert_eq!(unsafe { Controls::current() }, original);
    assert_ne!(translation(UPPER) & 1, 0);
    for _ in 0..40 {
        if unsafe { owner.advance_retirement() }.unwrap() == Progress::Retired {
            break;
        }
    }
    assert_eq!(unsafe { owner.advance_retirement() }, Ok(Progress::Retired));
    drop(owner);
    assert_eq!(control.current().unwrap().root(), None);
    tree.settle();
    kernel::metal::platform::boot::treaty::write(
        "PASS owned upper tables=5 active=1 access=1 original=1 retire=1 refund=5\n",
    );
}

//! Own, activate, and retire the first complete EL1 upper table tree.

use core::{arch::asm, ptr};

use kernel::{
    entry::boot::{Boot, Retirement},
    metal::{
        memory::mmu::{
            aarch64::internal::{context::Controls, installation::admit_inactive_upper},
            treaty::{LeafAccess, Memory, TableFormat, TableProgress},
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

use super::super::memory::{self, extent};

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
    let tree = unsafe { Tree::prepare(boot, range, evidence, TableFormat::Aarch64(profile)) };
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
    assert!(root.store.unlink_child(root.id, 0, Fast).is_err());
    assert!(active.finish(Fast).is_err());
    assert_eq!(tree.allowance.balance(), Ok(charged));

    unsafe { active.restore() }.unwrap();
    assert_eq!(unsafe { Controls::current() }, original);
    assert_ne!(translation(UPPER) & 1, 0);
    for _ in 0..32 {
        if unsafe { active.advance_retirement(Fast) }.unwrap() == TableProgress::Complete {
            break;
        }
    }
    assert_eq!(
        unsafe { active.advance_retirement(Fast) },
        Ok(TableProgress::Complete)
    );
    drop(active);
    assert_eq!(control.current().unwrap().root(), None);
    tree.settle();
    kernel::metal::platform::boot::treaty::write(
        "PASS owned upper tables=4 active=1 access=1 original=1 retire=1 refund=4\n",
    );
}

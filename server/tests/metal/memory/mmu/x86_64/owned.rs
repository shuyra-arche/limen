//! Retain the original firmware subtree while installing a real owned CR3 root.

use core::{
    arch::asm,
    ptr::{self, NonNull},
};

use kernel::{
    entry::boot::{Boot, Retirement},
    metal::{
        memory::mmu::{
            treaty::{LeafAccess, Memory, TableError, TableFormat, TableProgress},
            x86_64::internal::{context::Controls, installation::admit_root},
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{InternalAddress, PhysicalAddress, PhysicalRange};

use super::super::memory::{self, extent};

#[path = "../internal/owned.rs"]
mod tree;
use tree::Tree;
pub(super) const PAGES: usize = tree::PAGES;

const PAGE: u64 = 4096;
const INHERITED: usize = 0;
const COVERAGE: u64 = 1 << 39;
const ACCESSED: u64 = 1 << 5;

#[repr(C, align(4096))]
struct Witness([u64; 512]);

static WITNESS: Witness = Witness([0x43b1_0ace_d000_9876; 512]);
static REPLACEMENT: Witness = Witness([0x8c11_772e_3210_6543; 512]);
static mut WRITABLE: u64 = 0x1234;

/// # Safety
/// The actual q35 fixture retains the firmware's original identity-mapped
/// root and its live subtree. Other processors have not been started and
/// no device uses the independently qualified table-page allocation.
pub(super) unsafe fn check(boot: &Boot<'_>, range: PhysicalRange, evidence: &Retirement) {
    let original = unsafe { Controls::current() };
    let (mut control, format) = unsafe { admit_root(boot.processor()) }.unwrap();
    let original_context = control.current().unwrap();
    assert_eq!(
        original_context.root(),
        Some(PhysicalAddress::new(original.cr3 as u64))
    );
    assert!(original.pat.is_some());
    let old = NonNull::new(ptr::with_exposed_provenance_mut::<u64>(original.cr3)).unwrap();
    let before = unsafe { old.as_ptr().add(INHERITED).read_volatile() };
    assert_ne!(before & 1, 0);
    let slot = (1..256)
        .find(|&index| unsafe { old.as_ptr().add(index).read_volatile() } == 0)
        .expect("unused original root position");
    let alias = (slot as u64) << 39;

    let stack: usize;
    unsafe { asm!("mov {}, rsp", out(reg) stack, options(nostack, nomem)) };
    assert!(boot.stack_range().contains(&stack));
    assert!(boot.image().end().get() <= COVERAGE);
    assert!(range.end().get() <= COVERAGE && (original.cr3 as u64) < COVERAGE);

    let tree = unsafe { Tree::prepare(boot, range, evidence, TableFormat::X86_64(format)) };
    let [leaf, _, _, root] = tree.nodes;
    let witness = ptr::from_ref(&WITNESS).addr() as u64;
    assert!(boot.image().covers(extent(witness, witness + PAGE)));
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
    tree.link([0, 0, slot]);
    let charged = tree.allowance.balance().unwrap();
    let physical = root.store.cell(root.id).unwrap().1.start();

    // Wrong source and empty original slot cannot publish a shared
    // relation or freeze the private root.
    let wrong = NonNull::new(ptr::with_exposed_provenance_mut::<u64>(
        original.cr3 + PAGE as usize,
    ))
    .unwrap();
    assert!(matches!(
        unsafe {
            root.store
                .prepare_shared(root.id, &mut control, wrong, INHERITED, Fast)
        },
        Err(TableError::Scope)
    ));
    assert!(matches!(
        unsafe {
            root.store
                .prepare_shared(root.id, &mut control, old, slot, Fast)
        },
        Err(TableError::Scope)
    ));
    assert_eq!(root.store.descriptor(root.id, INHERITED, Fast), Ok(0));

    let mut active = unsafe {
        root.store
            .prepare_shared(root.id, &mut control, old, INHERITED, Fast)
    }
    .unwrap();
    let mut retired = None;
    assert_eq!(
        unsafe { root.store.retire_private(root.id, &mut retired, Fast) },
        Err(TableError::State)
    );
    assert!(retired.is_none());
    unsafe { active.install() }.unwrap();
    let current = unsafe { Controls::current() };
    assert_eq!(
        current,
        Controls {
            cr3: physical.get() as usize,
            ..original
        }
    );
    let value: u64;
    unsafe { asm!("mov {}, [{}]", out(reg) value, in(reg) alias, options(nostack, readonly)) };
    assert_eq!(value, WITNESS.0[0]);

    // Retain the actual live leaf slot, not a second private byte borrow.
    use kernel::metal::memory::mmu::treaty::{SwitchError, TranslationPhase};
    let address = InternalAddress::new(alias + PAGE);
    let target = PhysicalAddress::new(witness);
    let mut edit = active
        .prepare_translation(address, target, LeafAccess::ReadOnly, Fast)
        .unwrap();
    assert_eq!(edit.phase(), TranslationPhase::Prepared);
    assert!(matches!(
        active.prepare_translation(address, target, LeafAccess::ReadOnly, Fast),
        Err(kernel::metal::memory::mmu::treaty::TableError::Occupied)
    ));
    assert_eq!(unsafe { active.restore() }, Err(SwitchError::Current));
    unsafe { active.install_translation(&mut edit, Fast) }.unwrap();
    assert_eq!(edit.phase(), TranslationPhase::Active);
    let value: u64;
    unsafe { asm!("mov {}, [{}]", out(reg) value, in(reg) address.get(), options(nostack, readonly)) };
    assert_eq!(value, WITNESS.0[0]);
    unsafe { active.remove_translation(&mut edit, Fast) }.unwrap();
    assert_eq!(edit.phase(), TranslationPhase::Removed);
    let replacement_physical = ptr::from_ref(&REPLACEMENT).addr() as u64;
    assert!(boot.image().covers(extent(replacement_physical, replacement_physical + PAGE)));
    assert_ne!(replacement_physical, witness);
    let mut replacement = active
        .prepare_translation(
            address,
            PhysicalAddress::new(replacement_physical),
            LeafAccess::ReadOnly,
            Fast,
        )
        .unwrap();
    // Repeated retirement of the original cannot cancel the replacement.
    unsafe { active.remove_translation(&mut edit, Fast) }.unwrap();
    unsafe { active.install_translation(&mut replacement, Fast) }.unwrap();
    let value: u64;
    unsafe { asm!("mov {}, [{}]", out(reg) value, in(reg) address.get(), options(nostack, readonly)) };
    assert_eq!(value, REPLACEMENT.0[0]);
    unsafe { active.remove_translation(&mut replacement, Fast) }.unwrap();
    let mut cancelled = active
        .prepare_translation(address, target, LeafAccess::ReadOnly, Fast)
        .unwrap();
    unsafe { active.remove_translation(&mut cancelled, Fast) }.unwrap();
    assert_eq!(cancelled.phase(), TranslationPhase::Removed);

    unsafe {
        (&raw mut WRITABLE).write_volatile(0x5678);
        assert_eq!((&raw const WRITABLE).read_volatile(), 0x5678);
    }
    assert_eq!(
        root.store.unlink_child(root.id, slot, Fast),
        Err(TableError::State)
    );
    assert_eq!(active.finish(Fast), Err(TableError::State));
    assert_eq!(tree.allowance.balance(), Ok(charged));

    unsafe { active.restore() }.unwrap();
    assert_eq!(unsafe { Controls::current() }, original);
    // Restoration settles the hardware access, but the private root
    // must remain closed until its original retirement is complete.
    assert_eq!(
        unsafe { active.advance_retirement(Fast) },
        Ok(TableProgress::Pending)
    );
    assert_eq!(
        root.store.descriptor(root.id, INHERITED, Fast),
        Err(TableError::State)
    );
    let after = unsafe { old.as_ptr().add(INHERITED).read_volatile() };
    assert_eq!(after & !ACCESSED, before & !ACCESSED);
    assert_eq!(unsafe { old.as_ptr().add(slot).read_volatile() }, 0);
    assert_eq!(unsafe { (&raw const WRITABLE).read_volatile() }, 0x5678);

    // Native traversal sets Accessed on the retained owned branches.
    // The original branch matcher, not a fresh address, permits retirement.
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
    assert_eq!(control.current(), Some(original_context));
    tree.settle();
    kernel::metal::platform::boot::treaty::write(
        "PASS owned x86 tables=4 cr3=1 pat=1 borrowed=1 alias=1 original=1 retired-tree=1 refund=4\n",
    );
}

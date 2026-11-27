use super::{DATA, LAST, MIDDLE, NEXT, Page, TOP, physical};
use core::arch::asm;
use kernel::metal::memory::mmu::{
    aarch64::internal::{context::Controls, format::Root, installation::admit_root},
    treaty::{LeafAccess, Memory, Privilege, SwitchError},
};
use limen_treaty::model::PhysicalAddress;

#[path = "../../../../../support/paging/installation.rs"]
mod installation;

static EMPTY: Page = Page([0; 512]);

pub(crate) fn check(hart: usize) {
    let original = unsafe { Controls::current() };
    // AAVMF runs the fixture entirely through TTBR0. The unattached upper
    // root can be lent to this scenario before any other PE starts.
    assert!(original.tcr & (1 << 23) != 0 && original.ttbr1 == 0);
    let disabled = (original.tcr & !(0xffbf << 16)) | (0xb590 << 16);
    let enabled = disabled & !(1 << 23);
    unsafe {
        asm!("msr tcr_el1, {disabled}", "isb", "tlbi vmalle1is", "dsb ish", "isb",
            "msr ttbr1_el1, {empty}", "isb", "msr tcr_el1, {enabled}", "isb",
            disabled=in(reg) disabled, empty=in(reg) physical(&raw const EMPTY).get(),
            enabled=in(reg) enabled, options(nostack));
        assert!(matches!(
            admit_root(hart.wrapping_add(1), Root::Upper),
            Err(SwitchError::Context)
        ));
        let (mut control, format) = admit_root(hart, Root::Upper).unwrap();
        let current = Controls::current();
        TOP.0[0] = format.branch(physical(&raw const NEXT)).unwrap();
        NEXT.0[0] = format.branch(physical(&raw const MIDDLE)).unwrap();
        MIDDLE.0[0] = format.branch(physical(&raw const LAST)).unwrap();
        LAST.0[0] = format
            .leaf(
                physical(&raw const DATA),
                0,
                Privilege::Kernel,
                LeafAccess::ReadOnly,
                Memory::Normal,
            )
            .unwrap();
        let root = physical(&raw const TOP);
        let alias = 0xffff_0000_0000_0000u64;
        let candidate = control.prepare_root(root).unwrap();
        assert_eq!(candidate.root(), Some(root));
        assert!(matches!(
            control.prepare_root(PhysicalAddress::new(root.get() + 1)),
            Err(SwitchError::Scope)
        ));
        installation::roundtrip(&mut control, hart, candidate, |_| {
            assert_eq!(Controls::current().ttbr0, current.ttbr0);
            let data: u64;
            asm!("ldr {}, [{}]", out(reg) data, in(reg) alias, options(nostack, readonly));
            assert_eq!(data, DATA.0[0]);
            let write: u64;
            asm!("at s1e1w, {}", "isb", "mrs {}, par_el1", in(reg) alias, out(reg) write, options(nostack));
            assert_ne!(write & 1, 0);
        });
        assert_eq!(Controls::current(), current);
        drop(control);
        let retired: u64;
        asm!("at s1e1r, {}", "isb", "mrs {}, par_el1", in(reg) alias, out(reg) retired, options(nostack));
        assert_ne!(retired & 1, 0);
        asm!("msr tcr_el1, {disabled}", "isb", "dsb ishst", "tlbi vmalle1is", "dsb ish", "isb",
            "msr ttbr1_el1, {root}", "msr tcr_el1, {tcr}", "isb",
            disabled=in(reg) disabled, root=in(reg) original.ttbr1, tcr=in(reg) original.tcr, options(nostack));
    }
    assert_eq!(unsafe { Controls::current() }, original);
    kernel::metal::platform::boot::treaty::write(
        "PASS internal paging switched=1 alias=1 restored=1\n",
    );
    kernel::metal::platform::boot::treaty::write(
        "PASS local-context original=1 installed=1 restored=1\n",
    );
}

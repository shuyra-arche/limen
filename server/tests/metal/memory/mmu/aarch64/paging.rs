#[path = "../internal/pages.rs"]
mod pages;
use core::arch::asm;
use kernel::metal::memory::mmu::{
    aarch64::internal::{context::Controls, installation::admit_inactive_upper},
    treaty::{LeafAccess, Memory, Privilege, SwitchError},
};
use limen_treaty::model::PhysicalAddress;
use pages::{DATA, LAST, MIDDLE, NEXT, TOP, physical};

#[path = "../internal/installation.rs"]
mod installation;

pub(crate) fn check(processor: usize) {
    let original = unsafe { Controls::current() };
    // AAVMF runs the fixture entirely through TTBR0. The unattached upper
    // root can be lent to this scenario before any other PE starts.
    assert!(original.tcr & (1 << 23) != 0 && original.ttbr1 == 0);
    unsafe {
        assert!(matches!(
            admit_inactive_upper(processor.wrapping_add(1)),
            Err(SwitchError::Context)
        ));
        let (mut control, format) = admit_inactive_upper(processor).unwrap();
        assert_eq!(control.current().unwrap().root(), None);
        assert_eq!(Controls::current(), original);
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
        installation::roundtrip(&mut control, processor, candidate, |_| {
            let installed = Controls::current();
            assert_eq!(installed.ttbr0, original.ttbr0);
            assert_eq!(installed.tcr & 0xffff, original.tcr & 0xffff);
            assert_eq!(installed.mair, original.mair);
            assert_eq!(installed.sctlr, original.sctlr);
            assert!(matches!(
                admit_inactive_upper(processor),
                Err(SwitchError::Scope)
            ));
            let data: u64;
            asm!("ldr {}, [{}]", out(reg) data, in(reg) alias, options(nostack, readonly));
            assert_eq!(data, DATA.0[0]);
            let write: u64;
            asm!("at s1e1w, {}", "isb", "mrs {}, par_el1", in(reg) alias, out(reg) write, options(nostack));
            assert_ne!(write & 1, 0);
        });
        assert_eq!(Controls::current(), original);
        drop(control);
        let retired: u64;
        asm!("at s1e1r, {}", "isb", "mrs {}, par_el1", in(reg) alias, out(reg) retired, options(nostack));
        assert_ne!(retired & 1, 0);
    }
    assert_eq!(unsafe { Controls::current() }, original);
    kernel::metal::platform::boot::treaty::write(
        "PASS internal paging switched=1 alias=1 restored=1\n",
    );
    kernel::metal::platform::boot::treaty::write(
        "PASS local-context original=1 installed=1 restored=1\n",
    );
}

use super::{DATA, LAST, MIDDLE, NEXT, TOP, physical};
use core::{arch::asm, ptr};
use kernel::metal::memory::mmu::{
    LeafAccess, Memory, Privilege, SwitchError,
    x86_64::internal::{Controls, Translation},
};
use limen_treaty::model::PhysicalAddress;

pub(crate) fn check(hart: usize) {
    assert!(matches!(
        unsafe { Translation::admit(hart.wrapping_add(1)) },
        Err(SwitchError::Context)
    ));
    let mut translation = unsafe { Translation::admit(hart) }.expect("q35 four-level CPU paging");
    let original = translation.original();
    assert_eq!(original.cr3 & 0xfff, 0);
    let format = translation.format();
    // The fixture retains the identity-mapped firmware table pages.
    let old = ptr::with_exposed_provenance::<u64>(original.cr3);
    let top = unsafe { &raw mut TOP.0 };
    let root = physical(&raw const TOP);
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") 0x277u32, out("eax") low, out("edx") high, options(nomem, nostack));
        let pat = u64::from(high) << 32 | u64::from(low);
        for i in 0..512 {
            (*top)[i] = old.add(i).read_volatile();
        }
        let slot = (1..256)
            .find(|&i| (*top)[i] == 0)
            .expect("free lower PML4 entry");
        (*top)[slot] = format
            .branch(physical(&raw const NEXT), Privilege::Kernel)
            .unwrap();
        NEXT.0[0] = format
            .branch(physical(&raw const MIDDLE), Privilege::Kernel)
            .unwrap();
        MIDDLE.0[0] = format
            .branch(physical(&raw const LAST), Privilege::Kernel)
            .unwrap();
        LAST.0[0] = format
            .leaf(
                physical(&raw const DATA),
                0,
                Privilege::Kernel,
                LeafAccess::ReadOnly,
                Memory::Normal,
                pat,
            )
            .unwrap();
        translation.switch(original, root).unwrap();
        let data: u64;
        asm!("mov {}, [{}]", out(reg) data, in(reg) slot << 39, options(nostack, readonly));
        assert_eq!(data, DATA.0[0]);
        // Hardware set Accessed on this newly installed leaf.
        assert_ne!((&raw const LAST.0[0]).read_volatile() & (1 << 5), 0);
        assert_eq!(
            translation.switch(original, root),
            Err(SwitchError::Current)
        );
        translation
            .switch(
                translation.current().unwrap(),
                PhysicalAddress::new(original.cr3 as u64),
            )
            .unwrap();
        assert!(translation.pending().is_none());
    }
    assert_eq!(unsafe { Controls::current() }, original);
    kernel::entry::boot::write("PASS internal paging switched=1 alias=1 restored=1\n");
}

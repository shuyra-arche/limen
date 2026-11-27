use super::{DATA, LAST, MIDDLE, NEXT, TOP, physical};
use core::{arch::asm, ptr};
use kernel::metal::memory::mmu::internal::{
    LeafAccess, Memory, Privilege, SwitchError, x86_64::Controls,
};
use limen_treaty::model::PhysicalAddress;

pub(crate) fn check(hart: usize) {
    let original = unsafe { Controls::current() };
    assert_eq!(original.cr3 & 0xfff, 0);
    let format = original.local_format().expect("q35 four-level CPU paging");
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
        assert_eq!(
            original.switch(hart.wrapping_add(1), root),
            Err(SwitchError::Context)
        );
        original.switch(hart, root).unwrap();
        let data: u64;
        asm!("mov {}, [{}]", out(reg) data, in(reg) slot << 39, options(nostack, readonly));
        assert_eq!(data, DATA.0[0]);
        // Hardware set Accessed on this newly installed leaf.
        assert_ne!((&raw const LAST.0[0]).read_volatile() & (1 << 5), 0);
        assert_eq!(original.switch(hart, root), Err(SwitchError::Current));
        Controls::current()
            .switch(hart, PhysicalAddress::new(original.cr3 as u64))
            .unwrap();
    }
    assert_eq!(unsafe { Controls::current() }, original);
    kernel::entry::boot::write("PASS internal paging switched=1 alias=1 restored=1\n");
}

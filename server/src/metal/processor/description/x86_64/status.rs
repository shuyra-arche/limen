//! Value interpretation shared by x86 entry and machine consumers.

pub const fn kernel_selector(cs: u16) -> bool {
    cs & 3 == 0
}

pub const fn interrupts_masked(rflags: usize) -> bool {
    const IF: usize = 1 << 9;
    rflags & IF == 0
}

pub const fn paging(cr0: usize) -> bool {
    const PG: usize = 1 << 31;
    cr0 & PG != 0
}

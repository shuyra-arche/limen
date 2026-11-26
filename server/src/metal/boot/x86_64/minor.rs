//! Legacy xAPIC startup. The loader reserves one executable SIPI page
//! below 1 MiB. Its contents and the BSP's identity mappings stay resident.
use super::Descriptor;
use crate::metal::boot::minor::{self, Minor, StartError};
use core::{
    arch::{asm, naked_asm},
    mem::offset_of,
    ptr,
    sync::atomic::Ordering,
};

pub(crate) struct MinorContext;
pub(crate) const fn minor_context() -> MinorContext {
    MinorContext
}

/// The caller owns the low executable page and a timer valid after firmware exit.
/// The delay takes microseconds and reports whether the full interval elapsed.
#[derive(Clone, Copy)]
pub enum Startup {
    Xapic {
        trampoline: core::ptr::NonNull<[core::mem::MaybeUninit<u8>; 4096]>,
        delay: fn(u32) -> bool,
    },
}
const CODE: usize = 128;
const PARAMS: usize = CODE;
#[repr(C, packed)]
struct Parameters {
    gdt_limit: u16,
    gdt_base: u32,
    cr4: u32,
    cr3: u32,
    efer: u32,
    cr0: u32,
    slot: u32,
    destination: u32,
    selector: u16,
}
const _: () = assert!(PARAMS + size_of::<Parameters>() <= 4096);

fn rdmsr(index: u32) -> u64 {
    let (lo, hi): (u32, u32);
    unsafe { asm!("rdmsr", in("ecx") index,out("eax") lo,out("edx") hi,options(nostack,nomem)) };
    u64::from(lo) | (u64::from(hi) << 32)
}
fn idle(icr: *mut u32) -> bool {
    (0..100_000).any(|_| unsafe { icr.read_volatile() } & (1 << 12) == 0)
}

pub(crate) unsafe fn wake_minor(
    slot: &Minor,
    hart: usize,
    method: Startup,
) -> Result<(), StartError> {
    let Startup::Xapic { trampoline, delay } = method;
    let page = trampoline.addr().get();
    let apic = rdmsr(0x1b);
    if !(4096..0x100000).contains(&page)
        || !page.is_multiple_of(4096)
        || hart >= 255
        || apic & 0xc00 != 0x800
    {
        return Err(StartError::Rejected(-2));
    }
    let mut gdt = Descriptor { limit: 0, base: 0 };
    let (cr0, cr3, cr4): (usize, usize, usize);
    let cs: u16;
    unsafe {
        asm!("sgdt [{}]",in(reg) &mut gdt,options(nostack));
        asm!("mov {},cr0","mov {},cr3","mov {},cr4","mov {:x},cs",out(reg) cr0,out(reg) cr3,out(reg) cr4,out(reg) cs,options(nomem,nostack));
    }
    let low = |v: usize| u32::try_from(v).map_err(|_| StartError::Rejected(-2));
    // The reset stub uses a 32-bit far pointer and four-level identity mappings.
    if cr4 & ((1 << 12) | (1 << 17)) != 0 {
        return Err(StartError::Rejected(-2));
    }
    let params = Parameters {
        gdt_limit: gdt.limit,
        gdt_base: low(gdt.base)?,
        // Preserve PAE, PGE, OSFXSR and OSXMMEXCPT for the first Rust frame.
        cr4: (cr4 as u32) & 0x6a0,
        cr3: low(cr3)?,
        efer: (rdmsr(0xc0000080) as u32 & (1 << 11)) | (1 << 8),
        cr0: cr0 as u32,
        slot: low(ptr::from_ref(slot).expose_provenance())?,
        destination: low(minor as *const () as usize)?,
        selector: cs,
    };
    let target = trampoline.cast::<u8>().as_ptr();
    unsafe {
        ptr::copy_nonoverlapping(reset_stub as *const u8, target, CODE);
        target
            .add(PARAMS)
            .cast::<Parameters>()
            .write_unaligned(params);
    }
    core::sync::atomic::fence(Ordering::SeqCst);
    let base = (apic as usize) & 0x000f_ffff_ffff_f000;
    let icr = ptr::without_provenance_mut::<u32>(base + 0x300);
    let destination = ptr::without_provenance_mut::<u32>(base + 0x310);
    if !idle(icr) || !delay(1) {
        return Err(StartError::Rejected(-1));
    }
    let send = |cmd| unsafe {
        destination.write_volatile((hart as u32) << 24);
        icr.write_volatile(cmd);
    };
    send(0xc500); // INIT, level-triggered, assert.
    if !idle(icr) || !delay(10_000) {
        return Err(StartError::Unconfirmed(-1));
    }
    send(0x8500); // Deassert INIT, then deliver the startup vector.
    if !idle(icr) {
        return Err(StartError::Unconfirmed(-1));
    }
    for _ in 0..2 {
        send(0x600 | (page as u32 >> 12));
        if !idle(icr) || !delay(200) {
            return Err(StartError::Unconfirmed(-1));
        }
    }
    Ok(())
}

// Position-independent reset code: DS addresses the SIPI page. The two
// explicitly encoded far operands use a 16-bit address and a 32-bit base/IP.
#[unsafe(naked)]
unsafe extern "C" fn reset_stub() -> ! {
    naked_asm!(
        ".code16",
        "2: cli",
        "cld",
        "mov ax,cs",
        "mov ds,ax",
        ".byte 0x66,0x0f,0x01,0x16",
        ".short {gdt}", // lgdt32 [disp16]
        "mov eax,dword ptr [{cr4}]",
        "mov cr4,eax",
        "mov eax,dword ptr [{cr3}]",
        "mov cr3,eax",
        "mov ecx,0xc0000080",
        "mov eax,dword ptr [{efer}]",
        "xor edx,edx",
        "wrmsr",
        "mov esi,dword ptr [{slot}]",
        "mov eax,dword ptr [{cr0}]",
        "mov cr0,eax",
        ".byte 0x66,0xff,0x2e",
        ".short {dest}", // jmp far m16:32
        ".fill {size} - (. - 2b),1,0x90",
        ".code64",
        gdt = const PARAMS,
        cr4 = const PARAMS+offset_of!(Parameters,cr4),
        cr3 = const PARAMS+offset_of!(Parameters,cr3),
        efer = const PARAMS+offset_of!(Parameters,efer),
        slot = const PARAMS+offset_of!(Parameters,slot),
        cr0 = const PARAMS+offset_of!(Parameters,cr0),
        dest = const PARAMS+offset_of!(Parameters,destination),
        size = const CODE,
    )
}

#[unsafe(naked)]
unsafe extern "sysv64" fn minor() -> ! {
    naked_asm!(
        "mov esi,esi",
        "mov eax,1",
        "cpuid",
        "shr ebx,24",
        "mov edi,ebx",
        "cmp dword ptr [rsi+{state}],{published}",
        "jne 2f",
        "cmp rdi,[rsi+{hart}]",
        "jne 2f",
        "mov eax,{published}",
        "mov ecx,{entered}",
        "lock cmpxchg [rsi+{state}],ecx",
        "jne 2f",
        "mov rsp,[rsi+{stack}]",
        "call {finish}",
        "2: cli",
        "hlt",
        "jmp 2b",
        hart = const minor::HART_OFFSET,
        state = const minor::STATE_OFFSET,
        stack = const minor::STACK_OFFSET,
        published = const minor::PUBLISHED,
        entered = const minor::ENTERED,
        finish = sym finish,
    )
}
unsafe extern "sysv64" fn finish(hart: usize, slot: &'static Minor) -> ! {
    unsafe { crate::metal::hart::clear_local() };
    // The major CPU has already published the complete, immutable diagnostic IDT.
    super::load_idt();
    minor::arrive(hart, slot)
}

impl Minor {
    pub(in crate::metal) fn established(&self, hart: usize) -> bool {
        hart == crate::metal::hart::boot_identity() && super::established()
    }
}

//! Temporary, non-nested interrupt reception for q35 target test guests.

use core::{
    arch::{asm, naked_asm},
    marker::PhantomData,
    sync::atomic::{AtomicU32, Ordering},
};
use kernel::metal::memory::access::treaty::device::DeviceWindow;

use uefi_fixture::q35::{Idtr, io};

static RECEIVED: AtomicU32 = AtomicU32::new(0);
static mut GATES: [[u8; 16]; 256] = [[0; 16]; 256];

/// Prepare the exclusive local APIC window for an IRQ in priority class 15.
///
/// # Safety
/// The guest owns both legacy PICs and its original processor's UC xAPIC mapping.
pub unsafe fn prepare(window: &mut DeviceWindow<'_>) {
    unsafe {
        io::write8(0x21, 0xff);
        io::write8(0xa1, 0xff);
        window.write_u32(0x80, 0xe0).unwrap();
        window.write_u32(0xf0, 0x1ff).unwrap();
    }
}

#[unsafe(naked)]
unsafe extern "C" fn receive() -> ! {
    // The gate's IF remains clear. Clear the saved IF for the return.
    naked_asm!(
        "inc dword ptr [rip + {count}]",
        "and qword ptr [rsp + 16], -513",
        "iretq",
        count = sym RECEIVED,
    );
}

/// Restores the original table when the test leaves the reception window.
pub struct Window {
    original: Idtr,
    processor_local: PhantomData<*mut ()>,
}

impl Window {
    /// # Safety
    /// The current processor owns the diagnostic table and the single static
    /// fixture table. Leave interrupts masked outside this window. Set up
    /// the test's actual IRQ source with this exact vector.
    pub unsafe fn install(vector: u8) -> Self {
        assert!(vector >= 32);
        let original = Idtr::read();
        assert_eq!({ original.limit }, 4095);
        let gates = (&raw mut GATES).cast::<[u8; 16]>();
        unsafe { gates.copy_from_nonoverlapping(original.base as *const [u8; 16], 256) };
        let gate = unsafe { &mut *gates.add(usize::from(vector)) };
        let address = receive as *const () as u64;
        gate[..2].copy_from_slice(&(address as u16).to_le_bytes());
        gate[6..8].copy_from_slice(&((address >> 16) as u16).to_le_bytes());
        gate[8..12].copy_from_slice(&((address >> 32) as u32).to_le_bytes());
        let temporary = Idtr {
            limit: original.limit,
            base: gates.addr(),
        };
        RECEIVED.store(0, Ordering::Relaxed);
        unsafe { asm!("lidt [{}]", in(reg) &temporary, options(nostack, preserves_flags)) };
        Self {
            original,
            processor_local: PhantomData,
        }
    }

    pub fn count(&self) -> u32 {
        RECEIVED.load(Ordering::Acquire)
    }

    pub fn permit(&self) {
        unsafe { asm!("sti", "nop", "cli", options(nostack)) };
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe { asm!("lidt [{}]", in(reg) &self.original, options(nostack, preserves_flags)) };
    }
}

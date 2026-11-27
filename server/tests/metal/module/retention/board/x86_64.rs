use core::arch::{asm, x86_64::__cpuid};
use kernel::{metal::module::treaty::Machine, metal::platform::firmware::uefi::DeviceTree};
use uefi::proto::pi::mp::MpServices;

pub const MACHINE: Machine = Machine::X86_64;
pub const NAME: &str = "q35";

pub fn capture() -> Option<DeviceTree<'static>> {
    let handle = uefi::boot::get_handle_for_protocol::<MpServices>().unwrap();
    let mp = uefi::boot::open_protocol_exclusive::<MpServices>(handle).unwrap();
    let processors = mp.get_number_of_processors().unwrap();
    assert_eq!((processors.total, processors.enabled), (1, 1));
    None
}

pub fn hart() -> usize {
    (__cpuid(1).ebx >> 24) as usize
}

pub fn write_byte(byte: u8) -> bool {
    unsafe {
        asm!("out dx, al", in("dx") 0xe9u16, in("al") byte, options(nomem, nostack, preserves_flags))
    };
    true
}

pub fn reset(success: bool) {
    unsafe {
        asm!("out dx, eax", in("dx") 0xf4u16, in("eax") if success { 0x10u32 } else { 0x11u32 },
        options(nomem, nostack, preserves_flags))
    };
}

/// The caller has validated the entry and published its executable image.
pub unsafe fn query(entry: usize) -> u64 {
    let query = unsafe { core::mem::transmute::<usize, unsafe extern "sysv64" fn() -> u64>(entry) };
    unsafe { query() }
}

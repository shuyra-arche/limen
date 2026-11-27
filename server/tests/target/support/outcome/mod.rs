//! T0 only: completion transport. Never linked into the production kernel.
use core::arch::asm;

pub(super) fn finish(passed: bool) -> ! {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        // ISA debug exit encodes (value << 1) | 1.
        // The existing ordinary shutdown uses 0x10/0x11, not these values.
        asm!(
            "out dx, eax",
            in("dx") 0xf4_u16,
            in("eax") if passed { 0x30_u32 } else { 0x31_u32 },
            options(nostack)
        );
    }
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    {
        let args = [0x20026_usize, if passed { 97 } else { 99 }];
        #[cfg(target_arch = "aarch64")]
        unsafe {
            asm!(
                "hlt #0xf000",
                inlateout("x0") 0x20_usize => _,
                in("x1") args.as_ptr(),
                options(nostack)
            );
        }
        #[cfg(target_arch = "riscv64")]
        unsafe {
            asm!(
                ".balign 16",
                ".option push",
                ".option norvc",
                "slli zero, zero, 0x1f",
                "ebreak",
                "srai zero, zero, 7",
                ".option pop",
                inlateout("a0") 0x20_usize => _,
                in("a1") args.as_ptr(),
                options(nostack)
            );
        }
    }
    loop {
        core::hint::spin_loop();
    }
}

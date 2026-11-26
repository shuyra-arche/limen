//! Inspect the live four-level x86 root used by this singleton QEMU guest.

use core::arch::{asm, x86_64::__cpuid};
use kernel::{
    metal::firmware::map::FirmwareMap,
    metal::{
        address::{PhysicalAddress, PhysicalRange, length},
        module::Permissions,
    },
};
use limen_treaty::model::ByteLength;

pub const PAGE: u64 = 4096;
const ENTRIES: u64 = 512;
const TABLE_LIMIT: usize = 2048;
const ENTRY_ADDRESS: u64 = 0x000f_ffff_ffff_f000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Root {
    register: u64,
    address_mask: u64,
}

#[derive(Debug)]
pub enum Error {
    Context,
    Capacity,
    UnreadableTable,
    InvalidEntry,
    Alias,
    Permissions,
    Incomplete,
}

impl Root {
    pub fn current() -> Result<Self, Error> {
        let (cr0, cr3, cr4, low): (u64, u64, u64, u32);
        unsafe {
            asm!("mov {}, cr0", "mov {}, cr3", "mov {}, cr4",
                out(reg) cr0, out(reg) cr3, out(reg) cr4, options(nomem, nostack, preserves_flags));
            asm!("rdmsr", in("ecx") 0xc000_0080u32, out("eax") low, out("edx") _,
                options(nomem, nostack, preserves_flags));
        }
        const PAGING_AND_WP: u64 = (1 << 31) | (1 << 16);
        const LONG_MODE_AND_NX: u32 = (1 << 10) | (1 << 11);
        const UNSUPPORTED: u64 = (1 << 12) | (1 << 17) | (1 << 22) | (1 << 24);
        if cr0 & PAGING_AND_WP != PAGING_AND_WP
            || low & LONG_MODE_AND_NX != LONG_MODE_AND_NX
            || cr4 & UNSUPPORTED != 0
            || __cpuid(0x8000_0000).eax < 0x8000_0008
        {
            return Err(Error::Context);
        }
        let bits = __cpuid(0x8000_0008).eax & 0xff;
        if !(32..=52).contains(&bits) {
            return Err(Error::Context);
        }
        let address_mask = ((1u64 << bits) - 1) & ENTRY_ADDRESS;
        if cr3 & !address_mask & !0x18 != 0 {
            return Err(Error::Context);
        }
        Ok(Self {
            register: cr3,
            address_mask,
        })
    }

    /// Verify the expected permissions and exclude all non-identity mappings
    /// of these physical pages, including overlaps with larger leaves.
    ///
    /// # Safety
    ///
    /// The trusted x86 UEFI fixture supplies identity-mapped RAM for the
    /// active tables covered by the checked map. It retains that root and
    /// those readable pages across handoff. The one executing CPU must
    /// prevent firmware callbacks and paging edits while this walk is active.
    pub unsafe fn check(
        self,
        map: FirmwareMap<'_>,
        physical: PhysicalRange,
        expected: impl Fn(u64) -> Permissions,
    ) -> Result<(), Error> {
        if Self::current()? != self {
            return Err(Error::Context);
        }
        // Invalidate global as well as non-global translations before
        // excluding aliases from a walk of the active root.
        unsafe { self.flush() };
        let mut walk = Walk {
            map,
            physical,
            expected,
            address_mask: self.address_mask,
            remaining: TABLE_LIMIT,
            observed: 0,
        };
        let effective = Flags {
            writable: true,
            executable: true,
            user: true,
        };
        unsafe { walk.table(self.register & self.address_mask, 3, 0, effective)? };
        if Self::current()? != self || walk.observed != length(physical).get() {
            return Err(Error::Incomplete);
        }
        Ok(())
    }

    unsafe fn flush(self) {
        const GLOBAL_PAGES: u64 = 1 << 7;
        let cr4: u64;
        unsafe { asm!("mov {}, cr4", out(reg) cr4, options(nostack, preserves_flags)) };
        if cr4 & GLOBAL_PAGES != 0 {
            unsafe {
                asm!("mov cr4, {}", "mov cr4, {}",
                    in(reg) cr4 & !GLOBAL_PAGES, in(reg) cr4, options(nostack, preserves_flags))
            };
        } else {
            unsafe {
                asm!("mov cr3, {}", in(reg) self.register, options(nostack, preserves_flags))
            };
        }
        let _ = __cpuid(0);
    }
}

#[derive(Clone, Copy)]
struct Flags {
    writable: bool,
    executable: bool,
    user: bool,
}

impl Flags {
    fn within(self, entry: u64) -> Self {
        Self {
            writable: self.writable && entry & (1 << 1) != 0,
            executable: self.executable && entry & (1 << 63) == 0,
            user: self.user && entry & (1 << 2) != 0,
        }
    }
}

struct Walk<'a, Expected> {
    map: FirmwareMap<'a>,
    physical: PhysicalRange,
    expected: Expected,
    address_mask: u64,
    remaining: usize,
    observed: u64,
}

impl<Expected: Fn(u64) -> Permissions> Walk<'_, Expected> {
    unsafe fn table(
        &mut self,
        address: u64,
        level: u32,
        virtual_prefix: u64,
        effective: Flags,
    ) -> Result<(), Error> {
        self.remaining = self.remaining.checked_sub(1).ok_or(Error::Capacity)?;
        let table = PhysicalAddress::new(address)
            .range(ByteLength::new(PAGE))
            .ok_or(Error::UnreadableTable)?;
        // Restrict every table read to ordinary RAM with a retained original
        // identity mapping. The chosen OVMF profile supplies this while booting.
        const READ_PROTECT: u64 = 1 << 13;
        if address == 0
            || !self.map.regions().any(|region| {
                matches!(region.kind, 1..=7)
                    && region.attributes & READ_PROTECT == 0
                    && region.physical.covers(table)
            })
        {
            return Err(Error::UnreadableTable);
        }

        let size = 1u64 << (12 + 9 * level);
        for index in 0..ENTRIES {
            let pointer = core::ptr::with_exposed_provenance::<u64>((address + 8 * index) as usize);
            let entry = unsafe { pointer.read_volatile() };
            if entry & 1 == 0 {
                continue;
            }
            let large = level != 0 && entry & (1 << 7) != 0;
            if entry & ENTRY_ADDRESS & !self.address_mask != 0 || (large && level == 3) {
                return Err(Error::InvalidEntry);
            }
            let address = entry & self.address_mask;
            let virtual_start = virtual_prefix | (index * size);
            let flags = effective.within(entry);
            if level == 0 || large {
                // Bit 12 is PAT in a large leaf, and is not its address.
                let base = address & !(size - 1);
                if large && address & (size - 1) & !PAGE != 0 {
                    return Err(Error::InvalidEntry);
                }
                self.leaf(base, size, virtual_start, flags)?;
            } else {
                unsafe { self.table(address, level - 1, virtual_start, flags)? };
            }
        }
        Ok(())
    }

    fn leaf(
        &mut self,
        base: u64,
        size: u64,
        virtual_start: u64,
        flags: Flags,
    ) -> Result<(), Error> {
        let leaf = PhysicalAddress::new(base)
            .range(ByteLength::new(size))
            .ok_or(Error::InvalidEntry)?;
        let Some(overlap) = self.physical.intersect(leaf) else {
            return Ok(());
        };
        let first = overlap.start().get();
        let last = overlap.end().get();
        // Sign-extend the 48-bit virtual address before comparing its identity.
        let canonical = ((virtual_start << 16) as i64 >> 16) as u64;
        if canonical.checked_add(first - base) != Some(first) {
            return Err(Error::Alias);
        }
        for page in (first..last).step_by(PAGE as usize) {
            let (write, execute) = match (self.expected)(page) {
                Permissions::ReadOnly => (false, false),
                Permissions::ReadWrite => (true, false),
                Permissions::ReadExecute => (false, true),
            };
            if flags.user || flags.writable != write || flags.executable != execute {
                return Err(Error::Permissions);
            }
        }
        self.observed += last - first;
        Ok(())
    }
}

/// Run an inspection with local maskable interrupts held off.
pub fn without_interrupts<T>(inspect: impl FnOnce() -> T) -> T {
    let flags: u64;
    unsafe { asm!("pushfq", "pop {}", "cli", out(reg) flags) };
    let result = inspect();
    if flags & (1 << 9) != 0 {
        unsafe { asm!("sti", options(nomem, nostack, preserves_flags)) };
    }
    result
}

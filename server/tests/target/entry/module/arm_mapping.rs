//! Inspect the firmware-owned EL1 stage-1 root of the singleton Arm virt guest.

use core::arch::asm;
use kernel::{
    address::{PhysicalAddress, PhysicalRange, length},
    entry::boot::FirmwareMap,
    module::Permissions,
};
use limen_treaty::model::ByteLength;

pub const PAGE: u64 = 4096;
const TABLE_ENTRIES: usize = 512;
const TABLE_LIMIT: usize = 2048;
const ADDRESS: u64 = 0x0000_ffff_ffff_f000;
const BASE_ADDRESS: u64 = 0x0000_ffff_ffff_fffe;
const PXN: u64 = 1 << 53;
const WXN: u64 = 1 << 19;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Root {
    control: u64,
    translation: u64,
    lower: u64,
    upper: u64,
    attributes: u64,
    output_mask: u64,
    level: u32,
    entries: usize,
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
        let level: u64;
        unsafe { asm!("mrs {}, CurrentEL", out(reg) level, options(nomem, nostack)) };
        if level != 4 {
            return Err(Error::Context);
        }

        let (control, translation, lower, upper, attributes, features): (
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
        );
        unsafe {
            asm!(
                "mrs {control}, SCTLR_EL1", "mrs {translation}, TCR_EL1",
                "mrs {lower}, TTBR0_EL1", "mrs {upper}, TTBR1_EL1",
                "mrs {attributes}, MAIR_EL1", "mrs {features}, ID_AA64MMFR0_EL1",
                control = out(reg) control,
                translation = out(reg) translation,
                lower = out(reg) lower,
                upper = out(reg) upper,
                attributes = out(reg) attributes,
                features = out(reg) features,
                options(nomem, nostack),
            );
        }
        const MMU: u64 = 1;
        const BIG_ENDIAN: u64 = 1 << 25;
        const EPD0: u64 = 1 << 7;
        const TG0: u64 = 3 << 14;
        const EPD1: u64 = 1 << 23;
        const TAGS: u64 = 3 << 37;
        const ACCESS_DIRTY: u64 = 3 << 39;
        const HIERARCHICAL_DISABLE: u64 = 3 << 41;
        const LARGE_DESCRIPTORS: u64 = 1 << 59;
        if control & (BIG_ENDIAN | MMU) != MMU
            || translation
                & (EPD0 | TG0 | TAGS | ACCESS_DIRTY | HIERARCHICAL_DISABLE | LARGE_DESCRIPTORS)
                != 0
            || translation & EPD1 == 0
            || lower & 1 != 0
            || (features >> 28) & 0xf != 0
        {
            return Err(Error::Context);
        }
        let ips = ((translation >> 32) & 7) as usize;
        let bits = *[32u32, 36, 40, 42, 44, 48].get(ips).ok_or(Error::Context)?;
        if ips as u64 > features & 0xf {
            return Err(Error::Context);
        }
        let physical_mask = (1u64 << bits) - 1;
        let virtual_bits = 64 - (translation & 0x3f) as u32;
        if !(25..=48).contains(&virtual_bits) {
            return Err(Error::Context);
        }
        let index_bits = virtual_bits - 12;
        let level = (index_bits - 1) / 9;
        let entries = 1 << (index_bits - 9 * level);
        let address = lower & BASE_ADDRESS;
        if address == 0
            || address & !physical_mask != 0
            || !address.is_multiple_of((entries * 8) as u64)
        {
            return Err(Error::Context);
        }

        Ok(Self {
            control,
            translation,
            lower,
            upper,
            attributes,
            output_mask: physical_mask & ADDRESS,
            level,
            entries,
        })
    }

    /// Verify live EL1 permissions and exclude every nonidentity alias.
    ///
    /// # Safety
    ///
    /// This fixture runs one PE on non-secure Arm virt with stage 2 absent.
    /// AAVMF owns the readable identity-mapped, ordinary RAM pages containing
    /// the current tables. The caller retains these pages across handoff and
    /// masks interrupts throughout the walk. No concurrent table editor is
    /// permitted, including firmware callbacks.
    pub unsafe fn check(
        self,
        map: FirmwareMap<'_>,
        physical: PhysicalRange,
        expected: impl Fn(u64) -> Permissions,
    ) -> Result<(), Error> {
        if Self::current()? != self
            || !physical.start().get().is_multiple_of(PAGE)
            || !physical.end().get().is_multiple_of(PAGE)
        {
            return Err(Error::Context);
        }
        // EPD1 stops walks through TTBR1. A local invalidation also retires
        // higher-range TLB hits and cached permissions from an earlier walk.
        invalidate_translations();
        let mut walk = Walk {
            root: self,
            map,
            physical,
            expected,
            remaining: TABLE_LIMIT,
            observed: 0,
        };
        let flags = Flags {
            writable: true,
            privileged_execute: true,
            user_data: true,
        };
        unsafe {
            walk.table(
                self.lower & BASE_ADDRESS,
                self.level,
                self.entries,
                0,
                flags,
            )?;
        }
        if Self::current()? != self || walk.observed != length(physical).get() {
            return Err(Error::Incomplete);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Flags {
    writable: bool,
    privileged_execute: bool,
    user_data: bool,
}

impl Flags {
    fn within(self, descriptor: u64) -> Self {
        Self {
            writable: self.writable && descriptor & (1 << 62) == 0,
            privileged_execute: self.privileged_execute && descriptor & (1 << 59) == 0,
            user_data: self.user_data && descriptor & (1 << 61) == 0,
        }
    }
}

struct Walk<'a, Expected> {
    root: Root,
    map: FirmwareMap<'a>,
    physical: PhysicalRange,
    expected: Expected,
    remaining: usize,
    observed: u64,
}

impl<Expected: Fn(u64) -> Permissions> Walk<'_, Expected> {
    unsafe fn table(
        &mut self,
        address: u64,
        level: u32,
        entries: usize,
        virtual_prefix: u64,
        effective: Flags,
    ) -> Result<(), Error> {
        self.remaining = self.remaining.checked_sub(1).ok_or(Error::Capacity)?;
        let table = PhysicalAddress::new(address)
            .range(ByteLength::new((entries * 8) as u64))
            .ok_or(Error::UnreadableTable)?;
        const READ_PROTECT: u64 = 1 << 13;
        if address == 0
            || !self.map.regions().any(|region| {
                matches!(region.kind, 1..=7)
                    && region.attributes & READ_PROTECT == 0
                    && region.physical.covers(table)
            })
            || !identity(translate(address, false), address)
        {
            return Err(Error::UnreadableTable);
        }

        let size = 1u64 << (12 + 9 * level);
        for index in 0..entries {
            let pointer =
                core::ptr::with_exposed_provenance::<u64>((address + 8 * index as u64) as usize);
            let entry = unsafe { pointer.read_volatile() };
            if entry & 1 == 0 {
                continue;
            }
            if entry & ADDRESS & !self.root.output_mask != 0 {
                return Err(Error::InvalidEntry);
            }
            let output = entry & self.root.output_mask;
            let virtual_start = virtual_prefix | (index as u64 * size);
            match (level, entry & 3) {
                (0, 3) | (1 | 2, 1) => {
                    self.leaf(output, size, virtual_start, entry, effective)?;
                }
                (1..=3, 3) => unsafe {
                    self.table(
                        output,
                        level - 1,
                        TABLE_ENTRIES,
                        virtual_start,
                        effective.within(entry),
                    )?;
                },
                _ => return Err(Error::InvalidEntry),
            }
        }
        Ok(())
    }

    fn leaf(
        &mut self,
        base: u64,
        size: u64,
        virtual_start: u64,
        entry: u64,
        effective: Flags,
    ) -> Result<(), Error> {
        if !base.is_multiple_of(size) {
            return Err(Error::InvalidEntry);
        }
        let leaf = PhysicalAddress::new(base)
            .range(ByteLength::new(size))
            .ok_or(Error::InvalidEntry)?;
        let Some(overlap) = self.physical.intersect(leaf) else {
            return Ok(());
        };
        if base != virtual_start {
            return Err(Error::Alias);
        }
        let writable = effective.writable && entry & (1 << 7) == 0;
        let executable = effective.privileged_execute
            && entry & PXN == 0
            && !(writable && self.root.control & WXN != 0);
        let user_data = effective.user_data && entry & (1 << 6) != 0;
        let attribute = self.root.attributes >> (8 * ((entry >> 2) & 7)) & 0xff;
        // AAVMF grants execution by clearing both XN bits. This witness is
        // scoped to EL1, and also checks that EL0 has no data access.
        if user_data || !normal(attribute) || entry & (1 << 10) == 0 {
            return Err(Error::Permissions);
        }

        let first = overlap.start().get();
        let last = overlap.end().get();
        for page in (first..last).step_by(PAGE as usize) {
            let expected = match (self.expected)(page) {
                Permissions::ReadOnly => (false, false),
                Permissions::ReadWrite => (true, false),
                Permissions::ReadExecute => (false, true),
            };
            let access = translate(page, true);
            let permitted = if writable {
                identity(access, page)
            } else {
                // PAR.F and a stage-1 permission fault at some table level.
                access & 1 != 0 && ((access >> 1) & 0x3c) == 0x0c
            };
            if (writable, executable) != expected
                || !identity(translate(page, false), page)
                || !permitted
            {
                return Err(Error::Permissions);
            }
        }
        self.observed += last - first;
        Ok(())
    }
}

fn normal(attribute: u64) -> bool {
    // High-nibble zero denotes Device memory in the supported Armv8 format.
    attribute & 0xf0 != 0
}

fn translate(address: u64, write: bool) -> u64 {
    let result: u64;
    macro_rules! probe {
        ($op:literal) => {
            unsafe {
                asm!(
                    "mrs {saved}, PAR_EL1",
                    $op,
                    "isb",
                    "mrs {result}, PAR_EL1",
                    "msr PAR_EL1, {saved}",
                    address = in(reg) address,
                    saved = out(reg) _,
                    result = lateout(reg) result,
                    options(nostack),
                )
            }
        };
    }
    if write {
        probe!("at s1e1w, {address}");
    } else {
        probe!("at s1e1r, {address}");
    }
    result
}

fn identity(par: u64, address: u64) -> bool {
    par & 1 == 0 && par & ADDRESS == address & ADDRESS && normal(par >> 56)
}

fn invalidate_translations() {
    // The profile has one PE. VMALLE1 reaches all local EL1 ASIDs.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack)
        )
    };
}

/// Run with local exceptions masked, then restore the original DAIF flags.
pub fn without_interrupts<T>(inspect: impl FnOnce() -> T) -> T {
    let saved: u64;
    unsafe { asm!("mrs {}, DAIF", "msr DAIFSet, #15", out(reg) saved, options(nostack)) };
    let result = inspect();
    unsafe { asm!("msr DAIF, {}", in(reg) saved, options(nostack)) };
    result
}

fn line_size(shift: u32) -> usize {
    let cache: u64;
    unsafe { asm!("mrs {}, CTR_EL0", out(reg) cache, options(nomem, nostack)) };
    let line = 4 << ((cache >> shift) & 0xf);
    assert!(line <= PAGE as usize);
    line
}

/// Clean the verified identity range to PoU before its permissions change.
pub fn clean(physical: PhysicalRange) {
    let line = line_size(16);
    for address in (physical.start().get()..physical.end().get()).step_by(line) {
        unsafe { asm!("dc cvau, {}", in(reg) address, options(nostack)) };
    }
    unsafe { asm!("dsb ish", options(nostack)) };
}

/// Retire translation entries and synchronize instruction fetch after publication.
pub fn synchronize(physical: PhysicalRange) {
    invalidate_translations();
    let line = line_size(0);
    for address in (physical.start().get()..physical.end().get()).step_by(line) {
        unsafe { asm!("ic ivau, {}", in(reg) address, options(nostack)) };
    }
    unsafe { asm!("dsb ish", "isb", options(nostack)) };
}

//! Image-owned initial and successor RV64 roots for the QEMU virt probe.

use core::{
    arch::asm,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use limen_treaty::model::{InternalAddress, InternalRange, PhysicalAddress};
use kernel::metal::mmu::internal::riscv::{LeafAccess, Mode, context::{self, Context}};

pub const PAGE: u64 = 4096;
const ENTRIES: usize = 512;
const TABLE_COUNT: usize = 24;
const ROOT_COUNT: usize = 2;
const VALID: u64 = 1 << 0;
const READ: u64 = 1 << 1;
const WRITE: u64 = 1 << 2;
const EXECUTE: u64 = 1 << 3;
const ACCESSED: u64 = 1 << 6;
const DIRTY: u64 = 1 << 7;

#[repr(C, align(4096))]
struct Table([AtomicU64; ENTRIES]);

impl Table {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; ENTRIES])
    }

    fn is_empty(&self) -> bool {
        self.0.iter().all(|pte| read(pte) == 0)
    }
}

static TABLES: [[Table; TABLE_COUNT]; ROOT_COUNT] =
    [const { [const { Table::new() }; TABLE_COUNT] }; ROOT_COUNT];
static CLAIMED: [AtomicBool; ROOT_COUNT] = [const { AtomicBool::new(false) }; ROOT_COUNT];
const _: () = assert!(size_of::<Table>() == PAGE as usize);

pub fn backing() -> Range<u64> {
    let start = TABLES.as_ptr().addr() as u64;
    start..start + size_of_val(&TABLES) as u64
}

fn nonleaf(address: u64) -> u64 {
    (address / PAGE) << 10 | VALID
}

fn leaf(address: u64, permission: LeafAccess) -> u64 {
    let flags = match permission {
        LeafAccess::ReadOnly => READ,
        LeafAccess::ReadWrite => READ | WRITE | DIRTY,
        LeafAccess::ReadExecute => READ | EXECUTE,
    };
    (address / PAGE) << 10 | VALID | ACCESSED | flags
}

fn read(pte: &AtomicU64) -> u64 {
    pte.load(Ordering::Acquire)
}

fn write(pte: &AtomicU64, bits: u64) {
    pte.store(bits, Ordering::Release);
}

/// Move the calling Hart onto its next verified root and synchronize fetch.
///
/// The caller keeps a complete live identity path for the next context and
/// establishes both page-table and instruction-store visibility on this Hart.
pub unsafe fn switch_context(previous: Context, next: Context) {
    assert_ne!(previous, next);
    check_interrupts();
    unsafe {
        context::switch(previous, next).unwrap();
        asm!("fence.i", options(nostack));
    }
    check_interrupts();
}

fn check_interrupts() {
    let (status, enable): (u64, u64);
    unsafe {
        asm!(
            "csrr {status}, sstatus",
            "csrr {enable}, sie",
            status = out(reg) status,
            enable = out(reg) enable,
            options(nomem, nostack),
        )
    };
    assert_eq!(status & 2, 0);
    assert_eq!(enable, 0);
}

pub struct Root {
    mode: Mode,
    bank: usize,
    tables: &'static [Table; TABLE_COUNT],
    used: usize,
}

impl Root {
    pub fn new(mode: Mode) -> Self {
        assert_eq!(Context::current(), Some(Context::BARE));
        check_interrupts();
        Self::take(0, mode)
    }

    pub fn successor(&self) -> Self {
        self.check_active();
        assert!(core::ptr::eq(self.tables, &TABLES[0]));
        let next = Self::take(1, self.mode);
        assert!(self.storage().end <= next.storage().start);
        next
    }

    fn take(bank: usize, mode: Mode) -> Self {
        assert!(!CLAIMED[bank].swap(true, Ordering::AcqRel));
        let tables = &TABLES[bank];
        assert!(tables.iter().all(Table::is_empty));
        Self {
            mode,
            bank,
            tables,
            used: 1,
        }
    }

    fn address(&self, table: usize) -> u64 {
        (&self.tables[table] as *const Table).addr() as u64
    }

    pub fn storage(&self) -> Range<u64> {
        let start = self.address(0);
        start..start + size_of_val(self.tables) as u64
    }

    pub fn context(&self) -> Context {
        Context::paged(self.mode, PhysicalAddress::new(self.address(0))).unwrap()
    }

    fn low_end(&self) -> u64 {
        1 << (self.mode.virtual_bits() - 1)
    }

    fn child(&self, pte: u64) -> usize {
        let tables = self.storage();
        let address = (pte >> 10) << 12;
        assert!(tables.contains(&address));
        let index = ((address - tables.start) / PAGE) as usize;
        assert!(index < self.used);
        assert_eq!(pte, nonleaf(address));
        index
    }

    pub fn map(&mut self, range: Range<u64>, permission: LeafAccess) {
        self.map_with(range, permission, |address, permission| {
            permission.map_or_else(|| nonleaf(address), |permission| leaf(address, permission))
        });
    }

    /// Materialize an unpublished tree with the chosen RV64 backend.
    /// The mapping owner verifies the backend's result before every write.
    pub fn map_with(
        &mut self,
        range: Range<u64>,
        permission: LeafAccess,
        mut encode: impl FnMut(u64, Option<LeafAccess>) -> u64,
    ) {
        self.check_inactive();
        assert!(range.start < range.end && range.end <= self.low_end());
        assert!(self.mode.is_canonical_range(
            InternalRange::new(
                InternalAddress::new(range.start),
                InternalAddress::new(range.end),
            )
            .unwrap()
        ));
        assert!(range.start.is_multiple_of(PAGE) && range.end.is_multiple_of(PAGE));
        for address in range.step_by(PAGE as usize) {
            let entry = self.slot(address, 0, |branch| encode(branch, None));
            let pte = encode(address, Some(permission));
            assert_eq!(pte, leaf(address, permission));
            write(entry, pte);
        }
    }

    // Only the private, single-Hart geometry probe installs aliases. The
    // lifecycle scenarios continue to require exact identity mappings.
    pub fn map_probe(&mut self, address: u64, physical: PhysicalAddress, level: usize) {
        self.check_inactive();
        let bytes = self.mode.leaf_size(level).unwrap().get();
        assert!(address.is_multiple_of(bytes));
        let pte = self
            .mode
            .leaf(physical, level, LeafAccess::ReadOnly)
            .unwrap();
        write(self.slot(address, level, nonleaf), pte);
    }

    fn slot(
        &mut self,
        address: u64,
        leaf_level: usize,
        mut branch: impl FnMut(u64) -> u64,
    ) -> &AtomicU64 {
        let mut table = 0;
        for level in (leaf_level + 1..self.mode.levels()).rev() {
            let entry = &self.tables[table].0[self.mode.index(InternalAddress::new(address), level).unwrap()];
            let pte = read(entry);
            table = if pte == 0 {
                let next = self.used;
                let address = self.address(next);
                let pte = branch(address);
                assert_eq!(pte, nonleaf(address));
                write(entry, pte);
                self.used += 1;
                next
            } else {
                self.child(pte)
            };
        }
        let entry = &self.tables[table].0[self.mode.index(InternalAddress::new(address), leaf_level).unwrap()];
        assert_eq!(read(entry), 0);
        entry
    }

    /// The caller has checked identity mappings for the live image and DTB.
    pub unsafe fn activate(&self) {
        unsafe { switch_context(Context::BARE, self.context()) };
    }

    fn check_active(&self) {
        assert_eq!(Context::current(), Some(self.context()));
        check_interrupts();
    }

    fn check_inactive(&self) {
        assert_ne!(Context::current(), Some(self.context()));
        check_interrupts();
    }

    fn entry(&self, page: u64) -> &AtomicU64 {
        assert!(page < self.low_end() && page.is_multiple_of(PAGE));
        let mut table = 0;
        for level in (1..self.mode.levels()).rev() {
            table = self.child(read(
                &self.tables[table].0[self.mode.index(InternalAddress::new(page), level).unwrap()],
            ));
        }
        &self.tables[table].0[self.mode.index(InternalAddress::new(page), 0).unwrap()]
    }

    /// The local mapping owner withdraws after all other Harts have departed.
    pub fn withdraw(&self, range: Range<u64>, expected: impl Fn(u64) -> Option<LeafAccess>) {
        self.check_active();
        self.remove(range, expected);
        unsafe { asm!("sfence.vma x0, x0", "fence.i", options(nostack)) };
    }

    /// Clear exactly the old module mappings in the dormant root.
    ///
    /// Every Hart which previously used this root must have departed and
    /// completed its old translation and instruction maintenance. The
    /// caller keeps the root out of service throughout this operation.
    pub unsafe fn withdraw_inactive(
        &self,
        range: Range<u64>,
        expected: impl Fn(u64) -> Option<LeafAccess>,
    ) {
        self.check_inactive();
        self.remove(range, expected);
    }

    /// Erase root and branch pages after all Harts have left and fenced.
    ///
    /// The caller must have excluded subsequent use of this root on all
    /// Harts before reusing or releasing its retained table storage.
    pub unsafe fn clear_inactive(&mut self) {
        self.check_inactive();
        for table in &self.tables[..self.used] {
            for entry in &table.0 {
                write(entry, 0);
            }
        }
        assert!(self.tables.iter().all(Table::is_empty));
        self.used = 1;
    }

    pub fn release(self) {
        self.check_inactive();
        assert_eq!(self.used, 1);
        assert!(self.tables.iter().all(Table::is_empty));
        CLAIMED[self.bank].store(false, Ordering::Release);
    }

    fn remove(&self, range: Range<u64>, expected: impl Fn(u64) -> Option<LeafAccess>) {
        for page in range.clone().step_by(PAGE as usize) {
            assert_eq!(
                read(self.entry(page)),
                expected(page).map_or(0, |permission| leaf(page, permission))
            );
        }
        for page in range.step_by(PAGE as usize) {
            write(self.entry(page), 0);
        }
    }

    pub fn publish(&self, start: u64, pages: &[Option<LeafAccess>]) {
        self.check_active();
        for (index, permission) in pages.iter().copied().enumerate() {
            let page = start + index as u64 * PAGE;
            let pte = self.entry(page);
            assert_eq!(read(pte), 0);
            if let Some(permission) = permission {
                write(pte, leaf(page, permission));
            }
        }
        unsafe { asm!("sfence.vma x0, x0", "fence.i", options(nostack)) };
    }

    pub fn verify_private(&self, count: u64, expected: impl Fn(u64) -> Option<LeafAccess>) {
        assert_eq!(Context::current(), Some(Context::BARE));
        self.verify_inactive(count, expected);
    }

    pub fn verify_inactive(&self, count: u64, expected: impl Fn(u64) -> Option<LeafAccess>) {
        self.check_inactive();
        self.verify(count, expected);
    }

    pub fn verify_active(&self, count: u64, expected: impl Fn(u64) -> Option<LeafAccess>) {
        self.check_active();
        self.verify(count, expected);
    }

    fn verify(&self, count: u64, expected: impl Fn(u64) -> Option<LeafAccess>) {
        let mut visited = [false; TABLE_COUNT];
        let found = self.walk(0, self.mode.levels() - 1, 0, &mut visited, &expected);
        assert_eq!(found, count);
        assert!(visited[..self.used].iter().all(|visited| *visited));
    }

    /// Check every reachable entry, including addresses outside the image.
    /// Exact identity leaves rule out aliases of any protected physical page.
    fn walk(
        &self,
        table: usize,
        level: usize,
        base: u64,
        visited: &mut [bool; TABLE_COUNT],
        expected: &impl Fn(u64) -> Option<LeafAccess>,
    ) -> u64 {
        assert!(!visited[table], "duplicated branch table");
        visited[table] = true;
        let mut count = 0;
        for (index, pte) in self.tables[table].0.iter().enumerate() {
            let pte = read(pte);
            if pte == 0 {
                continue;
            }
            let address = base | ((index as u64) << shift(level));
            assert!(address < self.low_end());
            if level == 0 {
                let permission = expected(address).expect("unexpected mapping");
                assert_eq!(pte, leaf(address, permission));
                count += 1;
            } else {
                count += self.walk(self.child(pte), level - 1, address, visited, expected);
            }
        }
        count
    }
}

fn shift(level: usize) -> usize {
    12 + 9 * level
}

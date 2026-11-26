//! This probe owns an Sv39 root for one Hart on QEMU virt.

use core::{
    arch::asm,
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};

use kernel::module::Permissions;

pub const PAGE: u64 = 4096;
const ENTRIES: usize = 512;
const TABLE_COUNT: usize = 16;
const LOW_END: u64 = 1 << 38;
const SV39: u64 = 8 << 60;
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
}

static TABLES: [Table; TABLE_COUNT] = [const { Table::new() }; TABLE_COUNT];
const _: () = assert!(size_of::<Table>() == PAGE as usize);

pub fn backing() -> Range<u64> {
    let start = TABLES.as_ptr().addr() as u64;
    start..start + size_of_val(&TABLES) as u64
}

fn nonleaf(table: usize) -> u64 {
    let address = (&TABLES[table] as *const Table).addr() as u64;
    (address / PAGE) << 10 | VALID
}

fn leaf(address: u64, permission: Permissions) -> u64 {
    let flags = match permission {
        Permissions::ReadOnly => READ,
        Permissions::ReadWrite => READ | WRITE | DIRTY,
        Permissions::ReadExecute => READ | EXECUTE,
    };
    (address / PAGE) << 10 | VALID | ACCESSED | flags
}

fn read(pte: &AtomicU64) -> u64 {
    pte.load(Ordering::Acquire)
}

fn write(pte: &AtomicU64, bits: u64) {
    pte.store(bits, Ordering::Release);
}

fn satp() -> u64 {
    let value;
    unsafe { asm!("csrr {}, satp", out(reg) value, options(nomem, nostack)) };
    value
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
    used: usize,
}

impl Root {
    pub fn new() -> Self {
        assert_eq!(satp(), 0);
        check_interrupts();
        assert!(
            TABLES
                .iter()
                .all(|table| table.0.iter().all(|pte| read(pte) == 0))
        );
        Self { used: 1 }
    }

    fn context(&self) -> u64 {
        SV39 | (backing().start / PAGE)
    }

    fn child(&self, pte: u64) -> usize {
        let tables = backing();
        let address = (pte >> 10) << 12;
        assert!(tables.contains(&address));
        let index = ((address - tables.start) / PAGE) as usize;
        assert!(index < self.used);
        assert_eq!(pte, nonleaf(index));
        index
    }

    pub fn map(&mut self, range: Range<u64>, permission: Permissions) {
        assert_eq!(satp(), 0);
        assert!(range.start < range.end && range.end <= LOW_END);
        assert!(range.start.is_multiple_of(PAGE) && range.end.is_multiple_of(PAGE));
        for address in range.step_by(PAGE as usize) {
            let mut table = 0;
            for level in [2, 1] {
                let entry = &TABLES[table].0[index(address, level)];
                let pte = read(entry);
                table = if pte == 0 {
                    let next = self.used;
                    write(entry, nonleaf(next));
                    self.used += 1;
                    next
                } else {
                    self.child(pte)
                };
            }
            let entry = &TABLES[table].0[index(address, 0)];
            assert_eq!(read(entry), 0);
            write(entry, leaf(address, permission));
        }
    }

    /// The caller has checked identity mappings for the live image and DTB.
    pub unsafe fn activate(&self) {
        assert_eq!(satp(), 0);
        check_interrupts();
        unsafe {
            asm!(
                "sfence.vma x0, x0",
                "csrw satp, {context}",
                "sfence.vma x0, x0",
                context = in(reg) self.context(),
                options(nostack),
            )
        };
        self.check_active();
    }

    fn check_active(&self) {
        assert_eq!(satp(), self.context());
        check_interrupts();
    }

    fn entry(&self, page: u64) -> &AtomicU64 {
        assert!(page < LOW_END && page.is_multiple_of(PAGE));
        let mut table = 0;
        for level in [2, 1] {
            table = self.child(read(&TABLES[table].0[index(page, level)]));
        }
        &TABLES[table].0[index(page, 0)]
    }

    pub fn withdraw(&self, range: Range<u64>, expected: impl Fn(u64) -> Option<Permissions>) {
        self.check_active();
        for page in range.step_by(PAGE as usize) {
            let pte = self.entry(page);
            assert_eq!(
                read(pte),
                expected(page).map_or(0, |permission| leaf(page, permission))
            );
            write(pte, 0);
        }
        unsafe { asm!("sfence.vma x0, x0", "fence.i", options(nostack)) };
    }

    pub fn publish(&self, start: u64, pages: &[Option<Permissions>]) {
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

    pub fn verify_private(&self, count: u64, expected: impl Fn(u64) -> Option<Permissions>) {
        assert_eq!(satp(), 0);
        self.verify(count, expected);
    }

    pub fn verify_active(&self, count: u64, expected: impl Fn(u64) -> Option<Permissions>) {
        self.check_active();
        self.verify(count, expected);
    }

    fn verify(&self, count: u64, expected: impl Fn(u64) -> Option<Permissions>) {
        let mut visited = [false; TABLE_COUNT];
        let found = self.walk(0, 2, 0, &mut visited, &expected);
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
        expected: &impl Fn(u64) -> Option<Permissions>,
    ) -> u64 {
        assert!(!visited[table], "duplicated branch table");
        visited[table] = true;
        let mut count = 0;
        for (index, pte) in TABLES[table].0.iter().enumerate() {
            let pte = read(pte);
            if pte == 0 {
                continue;
            }
            let address = base | ((index as u64) << shift(level));
            assert!(address < LOW_END);
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

fn index(address: u64, level: usize) -> usize {
    (address >> shift(level)) as usize & (ENTRIES - 1)
}

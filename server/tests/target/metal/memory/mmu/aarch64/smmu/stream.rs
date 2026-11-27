//! Two EDU endpoints under the virt PCI host's checked stream mapping.
use core::arch::asm;
use kernel::metal::{
    memory::mmu::aarch64::external::smmu::{
        Binding, Context, Controller, Sequence, Storage, StreamTable,
        directory::Entry,
        translation::{Access, Format},
    },
    platform::description::devicetree::DeviceTree,
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

pub(super) const IOVA: u32 = 0x0020_0000;
pub(super) const PATTERNS: [u32; 2] = [0x9e55_e71c, 0x9ad0_815e];
pub(super) const SECOND: u32 = 7 << 3;
#[repr(C, align(4096))]
struct Page([u64; 512]);
#[repr(C, align(4096))]
struct Directory([Entry; 64]);
static mut STREAMS: Directory = Directory([const { Entry::EMPTY }; 64]);
static mut BINDINGS: [Option<Binding>; 2] = [None; 2];
static mut SEQUENCE: Sequence = Sequence::FRESH;
static mut TABLES: [[Page; 3]; 2] = [const { [const { Page([0; 512]) }; 3] }; 2];
static mut DATA: [Page; 2] = [const { Page([0; 512]) }; 2];

pub(super) fn order() {
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) }
}

fn address<T>(p: *mut T) -> PhysicalAddress {
    PhysicalAddress::new(p.addr() as u64)
}

pub(super) unsafe fn directory() -> (Storage<'static>, StreamTable) {
    let base = address(&raw mut STREAMS);
    (
        Storage {
            entries: unsafe { &mut *(&raw mut STREAMS.0) },
            physical: base,
            bindings: unsafe { &mut *(&raw mut BINDINGS) },
            sequence: unsafe { &mut *(&raw mut SEQUENCE) },
        },
        StreamTable { base, bits: 6 },
    )
}

#[path = "../../../../../support/pci/ecam.rs"]
mod pci;
use pci::Edu;
pub(super) use pci::RID;

pub(super) struct Endpoint {
    edu: Edu,
    index: usize,
    requester: u32,
}

impl Endpoint {
    pub(super) fn open(tree: &DeviceTree<'_>, expected: PhysicalRange, index: usize) -> Self {
        fn now() -> u64 {
            let value: u64;
            unsafe { asm!("mrs {}, cntvct_el0", out(reg) value, options(nostack)) };
            value
        }
        let hertz: u64;
        unsafe { asm!("mrs {}, cntfrq_el0", out(reg) hertz, options(nostack)) };
        let requester = [RID, SECOND][index];
        Self {
            edu: Edu::open(tree, expected, requester, None, order, now, hertz),
            index,
            requester,
        }
    }

    fn page(&self) -> *mut Page {
        unsafe { (&raw mut DATA).cast::<Page>().add(self.index) }
    }

    pub(super) fn word(&self) -> *mut u32 {
        self.page().cast()
    }

    pub(super) fn write(&mut self, iova: u32) {
        self.edu.transfer(iova, true);
    }

    pub(super) fn prime(&mut self) {
        let word = self.word();
        let physical = u32::try_from(word.addr()).unwrap();
        assert_ne!(physical, IOVA);
        unsafe { word.write_volatile(PATTERNS[self.index]) };
        order();
        self.edu.transfer(physical, false);
        unsafe { word.write_volatile(!PATTERNS[self.index]) };
        order();
        self.write(physical);
        assert_eq!(unsafe { word.read_volatile() }, PATTERNS[self.index]);
    }

    pub(super) fn verify_abort(&mut self) {
        let word = self.word();
        unsafe { word.write_volatile(!PATTERNS[self.index]) };
        order();
        self.write(word.addr() as u32);
        assert_eq!(unsafe { word.read_volatile() }, !PATTERNS[self.index]);
    }

    pub(super) fn prepare(&self, controller: &mut Controller<'_>) -> Binding {
        let format = controller.format().unwrap();
        let pages = unsafe { &mut (*(&raw mut TABLES))[self.index] };
        pages[0].0[0] = format.branch(address(&raw mut pages[1])).unwrap();
        pages[1].0[1] = format.branch(address(&raw mut pages[2])).unwrap();
        pages[2].0[0] = format
            .leaf(address(self.page()), Access::ReadWrite)
            .unwrap();
        order();
        let context = Context {
            stream: self.requester,
            root: address(&raw mut pages[0]),
            vmid: (self.index + 1) as u8,
        };
        unsafe { controller.prepare(context) }.unwrap()
    }

    pub(super) fn downgrade(&self, format: Format) {
        let leaf = format.leaf(address(self.page()), Access::Read).unwrap();
        unsafe {
            (&raw mut (*(&raw mut TABLES))[self.index][2].0[0]).write_volatile(leaf);
        }
        order();
    }
}

//! One EDU endpoint under the virt PCI host's actual stream mapping.
use core::arch::asm;
use kernel::metal::{
    memory::mmu::{
        external::smmu::{
            self,
            translation::{self, Access, Format},
        },
        internal::device::DeviceWindow,
    },
    platform::description::devicetree::DeviceTree,
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

const IOVA: u32 = 0x0020_0000;
const PATTERN: u32 = 0x9e55_e71c;
#[repr(C, align(4096))]
struct Page([u64; 512]);
static mut STREAMS: Page = Page([0; 512]);
static mut L1: Page = Page([0; 512]);
static mut L2: Page = Page([0; 512]);
static mut L3: Page = Page([0; 512]);
static mut DATA: Page = Page([0; 512]);

fn order() {
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) }
}
fn address(p: *mut Page) -> PhysicalAddress {
    PhysicalAddress::new(p.addr() as u64)
}
#[path = "../pci/edu.rs"]
mod pci;
use pci::{Edu, RID};

pub(super) struct Endpoint {
    edu: Edu,
    format: Option<Format>,
}

impl Endpoint {
    pub(super) fn open(tree: &DeviceTree<'_>, expected: PhysicalRange) -> Self {
        fn now() -> u64 {
            let value: u64;
            unsafe { asm!("mrs {}, cntvct_el0", out(reg) value, options(nostack)) };
            value
        }
        let hertz: u64;
        unsafe { asm!("mrs {}, cntfrq_el0", out(reg) hertz, options(nostack)) };
        Self {
            edu: Edu::open(tree, expected, false, order, now, hertz),
            format: None,
        }
    }
    fn word() -> *mut u32 {
        (&raw mut DATA).cast()
    }
    fn destination(&mut self, iova: u32, expected: u32) {
        unsafe { Self::word().write_volatile(!PATTERN) };
        order();
        self.edu.transfer(iova, true);
        assert_eq!(unsafe { Self::word().read_volatile() }, expected);
    }
    pub(super) fn prime(&mut self) {
        let pa = u32::try_from(Self::word().addr()).unwrap();
        assert_ne!(pa, IOVA);
        unsafe { Self::word().write_volatile(PATTERN) };
        order();
        self.edu.transfer(pa, false);
        self.destination(pa, PATTERN);
    }
    pub(super) fn verify_abort(&mut self) {
        self.destination(Self::word().addr() as u32, !PATTERN);
    }
    pub(super) fn install(&mut self, regs: &mut DeviceWindow, barrier: fn()) {
        let format = unsafe { Format::probe(regs).unwrap() };
        unsafe {
            (&raw mut STREAMS)
                .cast::<[u64; 8]>()
                .add(RID as usize)
                .write(format.stream(address(&raw mut L1), 1).unwrap());
            (&raw mut L1)
                .cast::<u64>()
                .write(format.branch(address(&raw mut L2)).unwrap());
            (&raw mut L2)
                .cast::<u64>()
                .add(1)
                .write(format.branch(address(&raw mut L3)).unwrap());
            (&raw mut L3).cast::<u64>().write(
                format
                    .leaf(address(&raw mut DATA), Access::ReadWrite)
                    .unwrap(),
            );
            order();
            let mut install =
                translation::install(regs, address(&raw mut STREAMS), 6, barrier).unwrap();
            for _ in 0..100_000 {
                match install.observe() {
                    Ok(()) => {
                        self.format = Some(format);
                        return;
                    }
                    Err(smmu::Error::Busy) => core::hint::spin_loop(),
                    Err(e) => panic!("SMMU enable {e:?}"),
                }
            }
        }
        panic!("SMMU enable timeout");
    }
    pub(super) fn verify_translation(&mut self) {
        self.destination(IOVA, PATTERN);
    }
    pub(super) fn downgrade(&self) {
        let leaf = self
            .format
            .as_ref()
            .unwrap()
            .leaf(address(&raw mut DATA), Access::Read)
            .unwrap();
        unsafe {
            (&raw mut L3).cast::<u64>().write_volatile(leaf);
        }
        order();
    }
    pub(super) fn verify_downgrade(&mut self) {
        self.destination(IOVA, !PATTERN);
    }
}

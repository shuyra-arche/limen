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
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress, PhysicalRange};

const RID: u32 = 6 << 3;
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
fn cell(bytes: &[u8], index: usize) -> u32 {
    u32::from_be_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

pub(super) struct Endpoint {
    bar: DeviceWindow,
    format: Option<Format>,
}

impl Endpoint {
    pub(super) fn open(tree: &DeviceTree<'_>, expected: PhysicalRange) -> Self {
        let mut hosts = tree.nodes().filter(|node| {
            node.compatible(b"pci-host-ecam-generic").unwrap() && node.enabled().unwrap()
        });
        let host = hosts.next().unwrap();
        assert!(hosts.next().is_none());
        let map = host.property(b"iommu-map").unwrap().unwrap();
        assert!(map.len() == 16 && cell(map, 0) == 0 && cell(map, 2) == 0 && cell(map, 3) > RID);
        assert!(host.property(b"iommu-map-mask").unwrap().is_none());
        assert_eq!(
            tree.phandle(cell(map, 1))
                .unwrap()
                .unwrap()
                .register(0)
                .unwrap(),
            expected
        );
        let ecam = host.register(0).unwrap();
        let reg = ecam.start().get() + u64::from(RID) * 4096;
        let cfg = PhysicalAddress::new(reg)
            .range(ByteLength::new(4096))
            .unwrap();
        assert!(ecam.covers(cfg));
        let mut cfg = unsafe { DeviceWindow::admit(cfg, InternalAddress::new(reg)).unwrap() };
        assert_eq!(unsafe { cfg.read_u32(0) }.unwrap(), 0x11e8_1234);
        let bar = u64::from(unsafe { cfg.read_u32(0x10) }.unwrap());
        assert_ne!(bar, 0);
        assert_eq!(bar % (1 << 20), 0);
        // Require the complete BAR to fall in an identity-translated 32-bit
        // PCI memory aperture in this exact virt host's seven-cell ranges.
        let ranges = host.property(b"ranges").unwrap().unwrap();
        assert!(ranges.len().is_multiple_of(28));
        assert!(ranges.chunks_exact(28).any(|range| {
            cell(range, 0) == 0x0200_0000
                && cell(range, 1) == 0
                && cell(range, 3) == 0
                && cell(range, 2) == cell(range, 4)
                && cell(range, 5) == 0
                && u64::from(cell(range, 2)) <= bar
                && bar + (1 << 20) <= u64::from(cell(range, 2)) + u64::from(cell(range, 6))
        }));
        let cmd = unsafe { cfg.read_u8(4).unwrap() };
        unsafe { cfg.write_u8(4, cmd | 6).unwrap() };
        order();
        assert_eq!(unsafe { cfg.read_u8(4).unwrap() } & 6, 6);
        let extent = PhysicalAddress::new(bar)
            .range(ByteLength::new(1 << 20))
            .unwrap();
        let mut bar = unsafe { DeviceWindow::admit(extent, InternalAddress::new(bar)).unwrap() };
        assert_eq!(unsafe { bar.read_u32(0).unwrap() } & 0xffff, 0xed);
        Self { bar, format: None }
    }

    fn transfer(&mut self, iova: u32, write: bool) {
        let (src, dst, cmd) = if write {
            (0x40000, iova, 3)
        } else {
            (iova, 0x40000, 1)
        };
        unsafe {
            assert_eq!(self.bar.read_u32(0x98).unwrap() & 1, 0);
            self.bar.write_u32(0x80, src).unwrap();
            self.bar.write_u32(0x88, dst).unwrap();
            self.bar.write_u32(0x90, 4).unwrap();
            order();
            self.bar.write_u32(0x98, cmd).unwrap();
            order();
            let now = || {
                let n: u64;
                asm!("mrs {}, cntvct_el0",out(reg)n,options(nostack));
                n
            };
            let freq: u64;
            asm!("mrs {}, cntfrq_el0",out(reg)freq,options(nostack));
            let start = now();
            while self.bar.read_u32(0x98).unwrap() & 1 != 0 {
                assert!(now().wrapping_sub(start) < 2 * freq, "EDU timeout");
            }
            order();
        }
    }
    fn word() -> *mut u32 {
        (&raw mut DATA).cast()
    }
    fn destination(&mut self, iova: u32, expected: u32) {
        unsafe { Self::word().write_volatile(!PATTERN) };
        order();
        self.transfer(iova, true);
        assert_eq!(unsafe { Self::word().read_volatile() }, expected);
    }
    pub(super) fn prime(&mut self) {
        let pa = u32::try_from(Self::word().addr()).unwrap();
        assert_ne!(pa, IOVA);
        unsafe { Self::word().write_volatile(PATTERN) };
        order();
        self.transfer(pa, false);
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

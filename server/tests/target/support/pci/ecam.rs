//! An EDU function on the virt PCI host with an identity requester map.
use kernel::metal::{
    memory::access::treaty::device::DeviceWindow,
    platform::description::treaty::devicetree::DeviceTree,
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress, PhysicalRange};

#[path = "edu.rs"]
mod edu;
use edu::Direction;

pub const RID: u32 = 6 << 3;
fn cell(bytes: &[u8], index: usize) -> u32 {
    u32::from_be_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

pub struct Edu(edu::Edu);

impl Edu {
    pub fn open(
        tree: &DeviceTree<'_>,
        expected: PhysicalRange,
        requester: u32,
        aperture_slot: Option<u8>,
        order: fn(),
        now: fn() -> u64,
        hertz: u64,
    ) -> Self {
        let mut hosts = tree.nodes().filter(|node| {
            node.compatible(b"pci-host-ecam-generic").unwrap() && node.enabled().unwrap()
        });
        let host = hosts.next().unwrap();
        assert!(hosts.next().is_none());
        let map = host.property(b"iommu-map").unwrap().unwrap();
        assert!(
            map.len() == 16 && cell(map, 0) == 0 && cell(map, 2) == 0 && cell(map, 3) > requester
        );
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
        let reg = ecam.start().get() + u64::from(requester) * 4096;
        let cfg = PhysicalAddress::new(reg)
            .range(ByteLength::new(4096))
            .unwrap();
        assert!(ecam.covers(cfg));
        let mut cfg = unsafe { DeviceWindow::admit(cfg, InternalAddress::new(reg)).unwrap() };
        assert_eq!(unsafe { cfg.read_u32(0) }.unwrap(), 0x11e8_1234);
        let saved = unsafe { cfg.read_u32(0x10) }.unwrap();
        assert_eq!(saved & 15, 0);
        let ranges = host.property(b"ranges").unwrap().unwrap();
        assert!(ranges.len().is_multiple_of(28));
        let mut apertures = ranges.chunks_exact(28).filter(|range| {
            cell(range, 0) == 0x0200_0000
                && cell(range, 1) == 0
                && cell(range, 3) == 0
                && cell(range, 2) == cell(range, 4)
                && cell(range, 5) == 0
        });
        let aperture = apertures.next().unwrap();
        assert!(apertures.next().is_none());
        let base = u64::from(cell(aperture, 2));
        let bar = if let Some(slot) = aperture_slot {
            // OpenSBI leaves each endpoint disabled and its 1 MiB BAR
            // unassigned. This fixture exclusively owns the 32-bit aperture.
            assert_eq!(saved, 0);
            assert_eq!(unsafe { cfg.read_u8(4) }.unwrap() & 6, 0);
            base + (u64::from(slot) << 20)
        } else {
            u64::from(saved)
        };
        assert!(
            bar != 0
                && bar.is_multiple_of(1 << 20)
                && bar >= base
                && bar + (1 << 20) <= base + u64::from(cell(aperture, 6))
        );
        if aperture_slot.is_some() {
            unsafe { cfg.write_u32(0x10, bar as u32).unwrap() };
            order();
            assert_eq!(unsafe { cfg.read_u32(0x10) }.unwrap(), bar as u32);
        }
        let cmd = unsafe { cfg.read_u8(4).unwrap() };
        unsafe { cfg.write_u8(4, cmd | 6).unwrap() };
        order();
        assert_eq!(unsafe { cfg.read_u8(4).unwrap() } & 6, 6);
        let extent = PhysicalAddress::new(bar)
            .range(ByteLength::new(1 << 20))
            .unwrap();
        let bar = unsafe { DeviceWindow::admit(extent, InternalAddress::new(bar)).unwrap() };
        Self(edu::Edu::admit(bar, order, now, hertz, u32::MAX))
    }

    pub fn transfer(&mut self, iova: u32, write: bool) {
        let direction = if write {
            Direction::ToRam
        } else {
            Direction::FromRam
        };
        // Each caller owns the one selected endpoint and its RAM fixture.
        unsafe { self.0.transfer(iova, direction) };
    }
}

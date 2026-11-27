//! GICR geometry and an independently held frame for each described PE.
use super::GicV3;
use crate::metal::memory::mmu::access::device::DeviceWindow;
use core::marker::PhantomData;
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

mod private;
mod state;
pub use private::{PrivateInterrupt, PrivateInterrupts};
use state::PrivateState;
#[cfg(all(limen_target, target_arch = "aarch64"))]
mod inputs;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub use inputs::{Ppi, StoppedPpi};

const TYPER: usize = 8;
const WAKER: usize = 0x14;
const PROCESSOR_SLEEP: u32 = 1 << 1;
const CHILDREN_ASLEEP: u32 = 1 << 2;
const LAST: u64 = 1 << 4;
const VIRTUAL_LPI: u64 = 1 << 1;
const FRAME_BYTES: usize = 0x2_0000;
const FRAME_ALIGNMENT: usize = 0x1_0000;
const FRAME_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Range,
    Capacity,
    Duplicate,
    Busy,
}

#[derive(Clone, Copy)]
struct Geometry {
    range: PhysicalRange,
    affinity: usize,
}

impl Geometry {
    fn from_typer(start: PhysicalAddress, typer: u64) -> Result<Self, Failure> {
        let bytes = if typer & VIRTUAL_LPI == 0 {
            FRAME_BYTES
        } else {
            2 * FRAME_BYTES
        };
        let packed = typer >> 32;
        let affinity = (packed & 0x00ff_ffff) | ((packed & 0xff00_0000) << 8);
        let affinity = usize::try_from(affinity).map_err(|_| Failure::Range)?;
        Ok(Self {
            range: start
                .range(ByteLength::new(bytes as u64))
                .ok_or(Failure::Range)?,
            affinity,
        })
    }
}

/// Scratch supplied by the owner that discovers the domain. Issued frames
/// no longer borrow the planning storage.
pub struct Slot(Option<Geometry>);

impl Slot {
    pub const EMPTY: Self = Self(None);
}

fn scan(
    region: PhysicalRange,
    stride: Option<usize>,
    rows: &mut [Slot],
    mut read: impl FnMut(usize) -> Result<u64, Failure>,
) -> Result<usize, Failure> {
    if rows.is_empty() || rows.len() > FRAME_LIMIT {
        return Err(Failure::Capacity);
    }
    let mut offset = 0usize;
    for index in 0..rows.len() {
        // A basic frame must fit before even its first register is read.
        let start = region
            .start()
            .get()
            .checked_add(offset as u64)
            .ok_or(Failure::Range)?;
        let start = PhysicalAddress::new(start);
        let basic = start
            .range(ByteLength::new(FRAME_BYTES as u64))
            .ok_or(Failure::Range)?;
        if !region.covers(basic) || !start.get().is_multiple_of(FRAME_ALIGNMENT as u64) {
            return Err(Failure::Range);
        }
        let typer = read(offset.checked_add(TYPER).ok_or(Failure::Range)?)?;
        let geometry = Geometry::from_typer(start, typer)?;
        if !region.covers(geometry.range) {
            return Err(Failure::Range);
        }
        if rows[..index].iter().any(|row| {
            row.0
                .is_some_and(|earlier| earlier.affinity == geometry.affinity)
        }) {
            return Err(Failure::Duplicate);
        }
        rows[index].0 = Some(geometry);
        let bytes = geometry.range.length().as_usize().ok_or(Failure::Range)?;
        let step = stride.unwrap_or(bytes);
        if step < bytes || !step.is_multiple_of(FRAME_ALIGNMENT) {
            return Err(Failure::Range);
        }
        if typer & LAST != 0 {
            return Ok(index + 1);
        }
        offset = offset.checked_add(step).ok_or(Failure::Range)?;
    }
    Err(Failure::Capacity)
}

/// Exclusive partition of the original GICR mapping. Each complete, checked
/// Frame can coexist with every other Frame from this domain.
pub struct Redistributor<'map, 'records> {
    domain: PhysicalRange,
    remaining: Option<DeviceWindow<'map>>,
    records: &'records [Slot],
    next: usize,
}

impl<'map, 'records> Redistributor<'map, 'records> {
    /// Scan the complete firmware region before issuing the first frame.
    ///
    /// # Safety
    /// The mapping is the original GICR region with readable little-endian
    /// TYPER registers and the caller owns its frame-allocation authority.
    /// Keep the original physical domain and mapping stable throughout use.
    pub unsafe fn admit(
        gic: GicV3<'_>,
        mut mapping: DeviceWindow<'map>,
        records: &'records mut [Slot],
    ) -> Result<Self, Failure> {
        let domain = gic.redistributor();
        if mapping.physical() != domain {
            return Err(Failure::Scope);
        }
        let count = scan(domain, gic.redistributor_stride, records, |offset| {
            unsafe { mapping.read_u64(offset) }
                .map(u64::from_le)
                .map_err(|_| Failure::Range)
        })?;
        Ok(Self {
            domain,
            remaining: Some(mapping),
            records: &records[..count],
            next: 0,
        })
    }
}

impl<'map> Iterator for Redistributor<'map, '_> {
    type Item = Frame<'map>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.records.len() {
            return None;
        }
        let geometry = self.records[self.next].0.expect("checked GICR geometry");
        self.next += 1;
        let mut remaining = self.remaining.take().expect("checked GICR remainder");
        let skip = geometry.range.start().get() - remaining.physical().start().get();
        if skip != 0 {
            let Ok((_, tail)) = remaining.split(ByteLength::new(skip)) else {
                unreachable!("checked GICR frame start")
            };
            remaining = tail;
        }
        let bytes = geometry.range.length();
        let mapping = if remaining.physical() == geometry.range {
            remaining
        } else {
            let Ok((head, tail)) = remaining.split(bytes) else {
                unreachable!("checked GICR frame length")
            };
            self.remaining = Some(tail);
            head
        };
        Some(Frame {
            geometry,
            domain: self.domain,
            mapping,

            private: [PrivateState::Vacant; 32],
        })
    }
}

/// Exclusive access to one exact GICR frame, retaining its original mapping.
#[must_use]
pub struct Frame<'map> {
    geometry: Geometry,
    domain: PhysicalRange,
    mapping: DeviceWindow<'map>,

    private: [PrivateState; 32],
}

impl<'map> Frame<'map> {
    pub fn affinity(&self) -> usize {
        self.geometry.affinity
    }

    pub fn physical(&self) -> PhysicalRange {
        self.geometry.range
    }

    pub fn domain(&self) -> PhysicalRange {
        self.domain
    }

    /// Move the original physical frame authority without moving its local
    /// virtual address. The backing lifetime is carried into the handoff.
    pub fn handoff(self) -> FrameHandoff<'map> {
        FrameHandoff {
            geometry: self.geometry,
            domain: self.domain,
            mapping: PhantomData,

            private: self.private,
        }
    }

    pub fn awake(&mut self) -> bool {
        awake(self.wake_bits())
    }

    fn wake_bits(&mut self) -> u32 {
        u32::from_le(unsafe { self.mapping.read_u32(WAKER) }.expect("checked GICR frame"))
    }

    /// Clear ProcessorSleep after ChildrenAsleep acknowledged entry to sleep.
    /// Poll the original PE until wake propagation finishes.
    #[cfg(all(limen_target, target_arch = "aarch64"))]
    pub fn wake(&mut self) -> Result<(), Failure> {
        if !super::cpu_interface::local(self.affinity()) {
            return Err(Failure::Scope);
        }
        let bits = self.wake_bits();
        if awake(bits) {
            return Ok(());
        }
        if bits & (PROCESSOR_SLEEP | CHILDREN_ASLEEP) == (PROCESSOR_SLEEP | CHILDREN_ASLEEP) {
            unsafe {
                self.mapping
                    .write_u32(WAKER, (bits & !PROCESSOR_SLEEP).to_le())
            }
            .expect("checked GICR frame");
            unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
        }
        Err(Failure::Busy)
    }
}

fn awake(bits: u32) -> bool {
    bits & (PROCESSOR_SLEEP | CHILDREN_ASLEEP) == 0
}

/// Portable ownership of an already partitioned frame. The source mapping
/// stays borrowed, and this object carries no executable or MMIO address.
#[must_use]
pub struct FrameHandoff<'origin> {
    geometry: Geometry,
    domain: PhysicalRange,
    mapping: PhantomData<&'origin mut ()>,

    private: [PrivateState; 32],
}

impl<'origin> FrameHandoff<'origin> {
    pub fn physical(&self) -> PhysicalRange {
        self.geometry.range
    }

    /// Attach the exact checked frame mapping independently admitted here.
    /// Failure returns the untouched frame authority and new local window.
    pub fn attach<'map>(
        self,
        mut mapping: DeviceWindow<'map>,
    ) -> Result<Frame<'map>, (Self, DeviceWindow<'map>, Failure)>
    where
        'origin: 'map,
    {
        let geometry = self.geometry;
        if mapping.physical() != geometry.range {
            return Err((self, mapping, Failure::Scope));
        }
        let typer = match unsafe { mapping.read_u64(TYPER) } {
            Ok(typer) => u64::from_le(typer),
            Err(_) => return Err((self, mapping, Failure::Range)),
        };
        if !Geometry::from_typer(geometry.range.start(), typer).is_ok_and(|actual| {
            actual.range == geometry.range && actual.affinity == geometry.affinity
        }) {
            return Err((self, mapping, Failure::Scope));
        }
        Ok(Frame {
            geometry,
            domain: self.domain,
            mapping,

            private: self.private,
        })
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/aarch64/gic/redistributor.rs"]
mod tests;

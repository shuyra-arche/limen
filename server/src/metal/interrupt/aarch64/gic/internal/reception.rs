//! Non-nested Group 1 acknowledgement on the PE that owns the CPU interface.
use core::arch::asm;

use super::super::GicV3;
use crate::metal::{
    memory::mmu::access::device::DeviceWindow,
    platform::description::devicetree::DeviceTree,
    processor::hart::aarch64::{native::boot_identity, status},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Special(u32),
}

/// A read of IAR takes ownership before software knows its INTID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Empty,
    Reading,
    Claimed(u32),
    DroppedPriority(u32),
}

/// One preallocated slot and the original PE's completion mode.
#[must_use]
pub struct Reception<'a> {
    redistributor: Redistributor<'a>,
    split: bool,
    state: State,
}

fn masked_el1() -> bool {
    let el: usize;
    let mask: usize;
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", out(reg) el, out(reg) mask, options(nostack, nomem))
    };
    status::el1(el) && mask & 0xc0 == 0xc0
}

fn mode() -> Option<bool> {
    let sre: u64;
    unsafe { asm!("mrs {}, ICC_SRE_EL1", out(reg) sre, options(nostack, nomem)) };
    if sre & 1 == 0 {
        return None;
    }
    let control: u64;
    let group: u64;
    unsafe {
        asm!("mrs {}, ICC_CTLR_EL1", "mrs {}, ICC_IGRPEN1_EL1", out(reg) control, out(reg) group, options(nostack, nomem))
    };
    (group & 1 != 0).then_some(control & (1 << 1) != 0)
}

/// The executing PE's Redistributor frame within its firmware window.
#[must_use]
pub struct Redistributor<'a> {
    mapping: &'a mut DeviceWindow,
    affinity: usize,
    wake_register: usize,
}

impl<'a> Redistributor<'a> {
    /// # Safety
    /// The caller owns the mapped GICR window and the executing PE's power
    /// handoff. Firmware supplies the frame layout and valid MMIO access.
    pub unsafe fn locate(
        gic: GicV3<'_>,
        tree: &DeviceTree<'_>,
        redistributors: &'a mut DeviceWindow,
    ) -> Result<Self, Failure> {
        if !masked_el1() || redistributors.physical() != gic.redistributor() {
            return Err(Failure::Scope);
        }
        let affinity = boot_identity();
        let node = tree.node(gic.source()).ok_or(Failure::Scope)?;
        let stride = node
            .property(b"redistributor-stride")
            .map_err(|_| Failure::Scope)?
            .map(|bytes| <[u8; 8]>::try_from(bytes).map(u64::from_be_bytes))
            .transpose()
            .map_err(|_| Failure::Scope)?;
        let mut offset = 0usize;
        let packed = (affinity & 0x00ff_ffff) | ((affinity >> 8) & 0xff00_0000);
        loop {
            let typer = u64::from_le(
                unsafe { redistributors.read_u64(offset.checked_add(8).ok_or(Failure::Scope)?) }
                    .map_err(|_| Failure::Scope)?,
            );
            if (typer >> 32) as usize == packed {
                let wake_register = offset.checked_add(0x14).ok_or(Failure::Scope)?;
                if !redistributors.supports_u32(wake_register) {
                    return Err(Failure::Scope);
                }
                return Ok(Self {
                    mapping: redistributors,
                    affinity,
                    wake_register,
                });
            }
            if typer & (1 << 4) != 0 {
                return Err(Failure::Scope);
            }
            let frame = if typer & 2 == 0 { 0x2_0000 } else { 0x4_0000 };
            let step = stride.unwrap_or(frame);
            if step < frame || !step.is_multiple_of(0x1_0000) {
                return Err(Failure::Scope);
            }
            offset = offset
                .checked_add(usize::try_from(step).map_err(|_| Failure::Scope)?)
                .ok_or(Failure::Scope)?;
        }
    }

    /// Clear ProcessorSleep only after ChildrenAsleep acknowledged entry
    /// into sleep. Poll this operation until wake propagation finishes.
    pub fn wake(&mut self) -> Result<(), Failure> {
        const PROCESSOR_SLEEP: u32 = 2;
        const CHILDREN_ASLEEP: u32 = 4;
        if !masked_el1() || boot_identity() != self.affinity {
            return Err(Failure::Scope);
        }
        let bits = u32::from_le(
            unsafe { self.mapping.read_u32(self.wake_register) }.expect("checked GICR frame"),
        );
        if bits & (PROCESSOR_SLEEP | CHILDREN_ASLEEP) == 0 {
            return Ok(());
        }
        if bits & (PROCESSOR_SLEEP | CHILDREN_ASLEEP) == (PROCESSOR_SLEEP | CHILDREN_ASLEEP) {
            unsafe {
                self.mapping
                    .write_u32(self.wake_register, (bits & !PROCESSOR_SLEEP).to_le())
            }
            .expect("checked GICR frame");
            unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
        }
        Err(Failure::Busy)
    }
}

impl<'a> Reception<'a> {
    /// # Safety
    /// The executing Non-secure PE owns the physical Group 1 interface
    /// and a non-nested, masked reception interval. Its system-register
    /// accesses are allowed by higher ELs. Keep its delivery mode, priority
    /// stack, isolation and Redistributor power state stable until all
    /// claims are settled. Source owners retain the binding across the
    /// IAR read. Only SGIs with active state may be routed in this profile.
    pub unsafe fn admit(redistributor: Redistributor<'a>) -> Result<Self, Failure> {
        if !masked_el1()
            || boot_identity() != redistributor.affinity
            || !unsafe { GicV3::cpu_interface_available() }
        {
            return Err(Failure::Scope);
        }
        let wake = unsafe { redistributor.mapping.read_u32(redistributor.wake_register) }
            .expect("checked GICR frame");
        if u32::from_le(wake) & 6 != 0 {
            return Err(Failure::Busy);
        }
        let split = mode().ok_or(Failure::Scope)?;
        Ok(Self {
            redistributor,
            split,
            state: State::Empty,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn context(&self) -> Result<(), Failure> {
        if !masked_el1()
            || self.redistributor.affinity != boot_identity()
            || mode() != Some(self.split)
        {
            return Err(Failure::Scope);
        }
        Ok(())
    }

    /// Resume work on the exact outstanding read, even if the temporary
    /// borrow returned at reception has already been released.
    pub fn claim(&mut self) -> Option<Claim<'_, 'a>> {
        let (State::Claimed(id) | State::DroppedPriority(id)) = self.state else {
            return None;
        };
        Some(Claim { owner: self, id })
    }

    pub fn receive(&mut self) -> Result<Option<Claim<'_, 'a>>, Failure> {
        self.context()?;
        if self.state != State::Empty {
            return Err(Failure::Busy);
        }
        self.state = State::Reading;
        let raw: u64;
        unsafe { asm!("mrs {}, ICC_IAR1_EL1", "dsb sy", out(reg) raw, options(nostack)) };
        let intid = (raw & 0xff_ffff) as u32;
        match intid {
            1020..=1023 => {
                self.state = State::Empty;
                if intid == 1023 {
                    Ok(None)
                } else {
                    Err(Failure::Special(intid))
                }
            }
            _ => {
                self.state = State::Claimed(intid);
                Ok(self.claim())
            }
        }
    }
}

/// An exclusive borrow of the actual IAR slot. While held it excludes any
/// later acknowledgement, including a new assertion of the same INTID.
#[must_use]
pub struct Claim<'slot, 'map> {
    owner: &'slot mut Reception<'map>,
    id: u32,
}

impl Claim<'_, '_> {
    pub fn intid(&self) -> u32 {
        self.id
    }

    /// # Safety
    /// The original claim is durably associated or quarantined. The owner
    /// permits priority drop and, in combined mode, deactivation now.
    pub unsafe fn drop_priority(&mut self) -> Result<(), Failure> {
        self.owner.context()?;
        let id = self.id;
        if self.owner.state != State::Claimed(id) {
            return Err(Failure::Busy);
        }
        unsafe {
            asm!("dsb sy", "msr ICC_EOIR1_EL1, {}", "isb", in(reg) u64::from(id), options(nostack))
        };
        self.owner.state = if self.owner.split && id < 8192 {
            State::DroppedPriority(id)
        } else {
            State::Empty
        };
        Ok(())
    }

    /// # Safety
    /// The original source is ready for deactivation. For level interrupts
    /// its assertion is cleared or its original route is durably masked.
    pub unsafe fn deactivate(&mut self) -> Result<(), Failure> {
        self.owner.context()?;
        let id = self.id;
        if self.owner.state != State::DroppedPriority(id) {
            return Err(Failure::Busy);
        }
        unsafe {
            asm!("dsb sy", "msr ICC_DIR_EL1, {}", "isb", in(reg) u64::from(id), options(nostack))
        };
        self.owner.state = State::Empty;
        Ok(())
    }
}

use crate::metal::{
    platform::boot::handoff::{Descriptions, Facts, uefi},
    processor::executor::aarch64::native,
};

mod major;
pub(crate) use major::major as enter_major;

pub(crate) unsafe fn admit<'a>(
    hart: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    unsafe { uefi::admit(hart, native::boot_identity(), input, descriptions) }
}

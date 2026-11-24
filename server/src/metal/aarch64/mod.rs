pub(crate) mod boot;

#[cfg(feature = "qemu-virt")]
pub(crate) mod pl011;

#[cfg(feature = "qemu-virt")]
pub(crate) mod firmware;

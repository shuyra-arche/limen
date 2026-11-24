pub(crate) mod boot;

#[cfg(feature = "qemu-virt")]
#[path = "board/virt.rs"]
mod board;

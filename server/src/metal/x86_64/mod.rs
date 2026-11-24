pub(crate) mod boot;

#[cfg(feature = "qemu-q35")]
#[path = "board/q35.rs"]
mod board;

//! Direct delivery to an actual supervisor interrupt-delivery context.

mod context;
pub(super) mod encoding;
mod reception;

pub use context::{Context, Destination};
pub use reception::{Claim, Receiver};

pub(super) const BASE: usize = 0x4000;
pub(super) const BYTES: usize = 32;

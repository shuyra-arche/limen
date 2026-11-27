//! Advanced Platform-Level Interrupt Controller: supervisor direct delivery.

mod description;
pub mod direct;
mod domain;
mod encoding;
mod registers;
mod source;

pub use description::{Description, DomainDescription, Mode, Route};
pub use domain::{Domain, Effect, Failure, State};
pub use source::Source;

const SOURCE_LIMIT: u32 = 1023;

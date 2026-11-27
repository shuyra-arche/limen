//! Common native test collection and terminal outcome.

use core::{fmt::Write, panic::PanicInfo};
use kernel::metal::platform::boot::treaty as diagnostic;

#[path = "../outcome/mod.rs"]
mod outcome;

#[path = "collection.rs"]
mod collection;
pub use collection::run;

pub fn finish() -> ! {
    outcome::finish(true)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(mut console) = diagnostic::console() {
        let _ = writeln!(console, "{info}");
    }
    outcome::finish(false)
}

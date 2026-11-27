#[path = "support/link.rs"]
mod link;

use std::path::Path;

fn main() {
    link::configure(&Path::new(env!("CARGO_MANIFEST_DIR")).join("support"));
}

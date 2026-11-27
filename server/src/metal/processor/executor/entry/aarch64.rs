use crate::metal::processor::{
    execution::terminal::{aarch64::vectors, trap as diagnostic},
    executor::{aarch64::native, local},
};

pub(super) use native::boot_identity as identity;

pub(super) fn verify() -> Option<local::Entry> {
    if !native::masked_stack() {
        return None;
    }
    local::Entry::admit(
        native::Entry {
            vector: vectors as *const () as usize,
        },
        diagnostic::stack_range(),
    )
}

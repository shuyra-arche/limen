//! Portable controls required by the native cooperative save shapes.

#[cfg_attr(
    not(any(test, all(limen_cooperative, target_arch = "aarch64"))),
    expect(
        dead_code,
        reason = "only AArch64 cooperative execution uses this save shape"
    )
)]
pub(super) mod aarch64;

#[cfg_attr(
    not(any(test, all(limen_cooperative, target_arch = "x86_64"))),
    expect(
        dead_code,
        reason = "only x86_64 cooperative execution uses this save shape"
    )
)]
pub(super) mod x86_64;

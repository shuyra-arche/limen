// Original private-input setup and phase retained by the frame and handoff.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Setup {
    pub(super) priority: u8,
    pub(super) inherited_priority: u8,
    pub(super) group: bool,
}

#[cfg_attr(
    not(all(limen_target, target_arch = "aarch64")),
    expect(
        dead_code,
        reason = "private-input setup and progress are observed at native admission"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PrivateState {
    Vacant,
    // Only hardware writes can establish any of the following states.
    Configuring(Setup),
    Masked(Setup),
    Enabling(Setup),
    Enabled(Setup),
    Masking(Setup),
    Restoring(Setup),
}

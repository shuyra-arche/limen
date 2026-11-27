use kernel::metal::memory::mmu::treaty::{Context, LocalContext, LocalInstall, SwitchError};

/// The original native owner already prepared both retained contexts.
pub(super) unsafe fn install(
    control: &mut LocalContext<'_>,
    expected: Context,
    candidate: Context,
) -> LocalInstall {
    assert_ne!(expected, candidate);
    assert_eq!(control.confirm(), Err(SwitchError::Current));
    assert_eq!(unsafe { control.retry() }, Err(SwitchError::Current));
    let installed = unsafe { control.install(expected, candidate) }.unwrap();
    assert_eq!(installed.previous(), expected);
    assert_eq!(installed.installed(), candidate);
    assert_eq!(control.current(), Some(candidate));
    assert_eq!(control.pending(), None);
    assert_eq!(
        unsafe { control.install(expected, candidate) },
        Err(SwitchError::Current)
    );
    installed
}

/// Exercise exact local installation, actual access, and restoration.
pub(super) unsafe fn roundtrip(
    control: &mut LocalContext<'_>,
    hart: usize,
    candidate: Context,
    observe: impl FnOnce(&mut LocalContext<'_>),
) {
    let original = control.current().unwrap();
    assert_eq!(
        unsafe { install(control, original, candidate) }.hart(),
        hart
    );
    observe(control);
    let previous = control.current().unwrap();
    assert_eq!(unsafe { install(control, previous, original) }.hart(), hart);
}

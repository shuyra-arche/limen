use kernel::metal::memory::mmu::{LocalRoot, SwitchError};
use limen_treaty::model::PhysicalAddress;

/// Both existing paging scenarios supply their bound authority and retained
/// offline table trees. The consumer exercises only the common operation.
pub(super) unsafe fn roundtrip(
    control: &mut LocalRoot,
    hart: usize,
    candidate: PhysicalAddress,
    observe: impl FnOnce(&mut LocalRoot),
) {
    let original = control.current().unwrap();
    assert_ne!(original, candidate);
    assert_eq!(unsafe { control.retry() }, Err(SwitchError::Current));

    let installed = unsafe { control.switch(original, candidate) }.unwrap();
    assert_eq!(installed.hart(), hart);
    assert_eq!(installed.previous(), original);
    assert_eq!(installed.installed(), candidate);
    assert_eq!(control.current(), Some(candidate));
    assert_eq!(
        unsafe { control.switch(original, candidate) },
        Err(SwitchError::Current)
    );
    observe(control);

    let previous = control.current().unwrap();
    let restored = unsafe { control.switch(previous, original) }.unwrap();
    assert_eq!(restored.hart(), hart);
    assert_eq!(restored.previous(), previous);
    assert_eq!(restored.installed(), original);
    assert_eq!(control.current(), Some(original));
    assert_eq!(control.pending(), None);
}

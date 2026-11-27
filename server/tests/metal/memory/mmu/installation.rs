use super::{Installation, SwitchError};
use crate::metal::memory::mmu::{aarch64, riscv64, x86_64};
use limen_treaty::model::PhysicalAddress;

#[test]
fn uncertain_readback_retains_the_original_operation() {
    let mut installation = Installation::new(3, 10);

    assert_eq!(installation.hart(), 3);
    assert_eq!(installation.retry(10), Err(SwitchError::Current));
    assert_eq!(installation.issue(9, 10, 20), Err(SwitchError::Current));
    assert_eq!(installation.issue(10, 9, 20), Err(SwitchError::Current));
    assert_eq!(installation.current(), Some(10));
    assert_eq!(installation.pending(), None);

    installation.issue(10, 10, 20).unwrap();
    assert_eq!(installation.current(), None);
    assert_eq!(installation.complete(10), Err(SwitchError::Unconfirmed));
    assert_eq!(
        installation.issue(10, 10, 30),
        Err(SwitchError::Unconfirmed)
    );
    assert_eq!(installation.retry(30), Err(SwitchError::Unconfirmed));
    assert_eq!(installation.retry(10), Ok(20));
    assert_eq!(installation.retry(20), Ok(20));
    assert_eq!(installation.pending(), Some((10, 20)));

    assert_eq!(installation.complete(20), Ok((10, 20)));
    assert_eq!(installation.current(), Some(20));
    assert_eq!(installation.complete(20), Err(SwitchError::Current));
}

#[test]
fn same_root_readback_does_not_excuse_a_changed_execution_profile() {
    let first = x86_64::internal::Controls {
        cr0: 0x8001_0000,
        cr3: 0x2000,
        cr4: 1 << 5,
        efer: (1 << 10) | (1 << 11),
    };
    let next = x86_64::internal::Controls {
        cr3: 0x3000,
        ..first
    };
    let wrong = x86_64::internal::Controls {
        efer: 1 << 10,
        ..next
    };
    let mut install = Installation::new(3, first);
    install.issue(first, first, next).unwrap();
    assert_eq!(install.complete(wrong), Err(SwitchError::Unconfirmed));
    assert_eq!(install.retry(wrong), Err(SwitchError::Unconfirmed));
    assert_eq!(install.retry(first), Ok(next));
    assert_eq!(install.complete(next), Ok((first, next)));
}

#[test]
fn an_unrelated_arm_root_change_cannot_complete_the_original_install() {
    let first = aarch64::internal::Controls {
        mair: 0xff,
        tcr: 0x0000_b510_b510,
        ttbr0: 0x1000,
        ttbr1: 0x2000,
        sctlr: 1,
    };
    let next = aarch64::internal::Controls {
        ttbr1: 0x3000,
        ..first
    };
    let wrong = aarch64::internal::Controls {
        ttbr0: 0x4000,
        ..next
    };
    let mut install = Installation::new(7, first);
    install.issue(first, first, next).unwrap();
    assert_eq!(install.complete(wrong), Err(SwitchError::Unconfirmed));
    assert_eq!(install.pending(), Some((first, next)));
    assert_eq!(install.retry(next), Ok(next));
    assert_eq!(install.complete(next), Ok((first, next)));
    install.issue(next, next, first).unwrap();
    assert_eq!(install.complete(first), Ok((next, first)));
}

#[test]
fn satp_readback_carries_its_exact_mode_even_when_the_root_is_unchanged() {
    use riscv64::{internal::context::Context, sv::Mode};

    let root = PhysicalAddress::new(0x2000);
    let previous = Context::paged(Mode::Sv39, root).unwrap();
    let requested = Context::paged(Mode::Sv48, root).unwrap();
    let unrelated = Context::paged(Mode::Sv57, root).unwrap();
    let mut install = Installation::new(2, previous);

    install.issue(previous, previous, requested).unwrap();
    assert_eq!(install.complete(previous), Err(SwitchError::Unconfirmed));
    assert_eq!(install.retry(unrelated), Err(SwitchError::Unconfirmed));
    assert_eq!(install.pending(), Some((previous, requested)));
    assert_eq!(install.retry(previous), Ok(requested));
    assert_eq!(install.complete(requested), Ok((previous, requested)));
}

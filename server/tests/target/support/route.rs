//! Exercise the original bound physical interrupt route.

use kernel::metal::interrupt::treaty::{RouteControl, RouteOperation, RouteState};

pub unsafe fn publish(route: &mut impl RouteControl) {
    assert_eq!(route.state(), RouteState::Prepared);
    assert!(unsafe { route.resume() }.is_err());
    for _ in 0..4096 {
        if unsafe { route.publish() }.is_ok() {
            assert_eq!(route.state(), RouteState::Open);
            assert!(unsafe { route.publish() }.is_err());
            return;
        }
        let pending = RouteState::Unconfirmed(RouteOperation::Publish);
        assert_eq!(route.state(), pending);
        assert!(unsafe { route.resume() }.is_err());
        assert_eq!(route.state(), pending);
    }
    panic!("original route publication was not confirmed");
}

pub fn mask(route: &mut impl RouteControl) {
    for _ in 0..4096 {
        if route.mask().is_ok() {
            assert_eq!(route.state(), RouteState::Masked);
            return;
        }
        assert_eq!(route.state(), RouteState::Unconfirmed(RouteOperation::Mask));
    }
    panic!("original route mask was not confirmed");
}

pub unsafe fn resume(route: &mut impl RouteControl) {
    assert_eq!(route.state(), RouteState::Masked);
    assert!(unsafe { route.publish() }.is_err());
    for _ in 0..4096 {
        if unsafe { route.resume() }.is_ok() {
            assert_eq!(route.state(), RouteState::Open);
            assert!(unsafe { route.resume() }.is_err());
            return;
        }
        let pending = RouteState::Unconfirmed(RouteOperation::Resume);
        assert_eq!(route.state(), pending);
        assert!(unsafe { route.publish() }.is_err());
        assert_eq!(route.state(), pending);
    }
    panic!("original route resume was not confirmed");
}

pub unsafe fn retire<R: RouteControl>(route: &mut R) -> R::Retired {
    assert_eq!(route.state(), RouteState::Masked);
    for _ in 0..4096 {
        if let Ok(retired) = unsafe { route.retire() } {
            assert_eq!(route.state(), RouteState::Retired);
            assert!(unsafe { route.retire() }.is_err());
            return retired;
        }
        assert_eq!(
            route.state(),
            RouteState::Unconfirmed(RouteOperation::Retire)
        );
    }
    panic!("original route retirement was not confirmed");
}

//! Exercise the original bound physical interrupt route.

use kernel::metal::interrupt::treaty::RouteControl;

pub unsafe fn publish(route: &mut impl RouteControl) {
    assert!(unsafe { route.publish() }.is_ok());
    assert!(unsafe { route.publish() }.is_err());
}

pub fn mask(route: &mut impl RouteControl) {
    for _ in 0..4096 {
        if route.mask().is_ok() {
            return;
        }
    }
    panic!("original route mask was not confirmed");
}

pub unsafe fn retire(route: &mut impl RouteControl) {
    assert!(unsafe { route.retire() }.is_ok());
    assert!(unsafe { route.retire() }.is_err());
}

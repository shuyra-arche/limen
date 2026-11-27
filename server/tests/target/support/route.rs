//! Exercise publication of an already prepared physical interrupt route.

use kernel::metal::interrupt::PublishRoute;

pub unsafe fn publish(route: &mut impl PublishRoute) {
    assert!(unsafe { route.publish() }.is_ok());
    assert!(unsafe { route.publish() }.is_err());
}

//! Exercise the original binding request, recovery and completion on the remapping unit.

use core::fmt::Debug;
use kernel::metal::memory::mmu::treaty::{
    BindingChange, BindingControl, BindingEffect, BindingProgress,
};

pub fn settle<C: BindingControl>(
    controller: &mut C,
    change: BindingChange<C>,
) -> BindingEffect<C::Binding, C::Refresh>
where
    C::Binding: Debug,
    C::Refresh: Debug,
    C::Command: Debug,
    C::Error: Debug,
{
    let binding = change.effect().binding();
    let recovered = controller
        .pending_change(binding)
        .expect("the original binding change remains observable");
    assert_eq!(recovered, change);
    assert_eq!(controller.unsettled_change(), Some(change));
    for _ in 0..100_000 {
        match controller.observe_change(recovered) {
            BindingProgress::Confirmed(effect) => {
                assert_eq!(effect, change.effect());
                assert_eq!(controller.pending_change(binding), None);
                assert!(matches!(
                    controller.observe_change(recovered),
                    BindingProgress::Stale
                ));
                assert!(matches!(
                    controller.observe_change(change),
                    BindingProgress::Stale
                ));
                return effect;
            }
            BindingProgress::Pending => core::hint::spin_loop(),
            other => panic!("original binding command: {other:?}"),
        }
    }
    panic!("the original binding command did not settle");
}

pub unsafe fn bind<C: BindingControl>(controller: &mut C, context: C::Context) -> C::Binding
where
    C::Binding: Debug,
    C::Refresh: Debug,
    C::Command: Debug,
    C::Error: Debug,
{
    let command = unsafe { controller.begin_binding(context) }.unwrap();
    let BindingEffect::Bound(binding) = settle(controller, command) else {
        panic!("expected binding publication");
    };
    binding
}

pub unsafe fn retire<C: BindingControl>(
    controller: &mut C,
    binding: C::Binding,
    after_start: impl FnOnce(&mut C),
) -> BindingChange<C>
where
    C::Binding: Debug,
    C::Refresh: Debug,
    C::Command: Debug,
    C::Error: Debug,
{
    let change = unsafe { controller.begin_retirement(binding) }.unwrap();
    after_start(controller);
    assert_eq!(settle(controller, change), BindingEffect::Unbound(binding));
    change
}

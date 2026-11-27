//! One exact binding retirement scenario over the admitted remapping unit.

use kernel::metal::memory::mmu::{Retirement, Unbind};

pub unsafe fn retire<C: Unbind>(
    controller: &mut C,
    binding: C::Binding,
    after_start: impl FnOnce(&mut C),
) -> C::Ticket
where
    C::Error: core::fmt::Debug,
{
    let ticket = unsafe { controller.retire(binding) }.unwrap();
    after_start(controller);
    let recovered = controller
        .pending_retirement(binding)
        .expect("the original retirement remains observable");
    for _ in 0..100_000 {
        if controller.observe_retirement(&recovered).unwrap() == Retirement::Settled {
            assert!(controller.pending_retirement(binding).is_none());
            assert!(controller.observe_retirement(&recovered).is_err());
            assert!(controller.observe_retirement(&ticket).is_err());
            return ticket;
        }
        core::hint::spin_loop();
    }
    panic!("the original unbind did not settle");
}

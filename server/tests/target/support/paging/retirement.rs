//! One exact binding retirement scenario over the admitted remapping unit.

use kernel::metal::memory::mmu::{Retirement, Unbind};

pub unsafe fn retire<C: Unbind>(controller: &mut C, binding: C::Binding)
where
    C::Error: core::fmt::Debug,
{
    let ticket = unsafe { controller.retire(binding) }.unwrap();
    for _ in 0..100_000 {
        if controller.observe_retirement(&ticket).unwrap() == Retirement::Settled {
            assert!(controller.observe_retirement(&ticket).is_err());
            return;
        }
        core::hint::spin_loop();
    }
    panic!("the original unbind did not settle");
}

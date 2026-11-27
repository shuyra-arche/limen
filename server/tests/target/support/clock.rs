//! The same elapsed-time observation over the clock supplied by each board.

use core::{hint::spin_loop, time::Duration};
use kernel::metal::chrono::treaty::Clock;

pub fn observe(clock: &Clock) {
    let initial = clock.sample().expect("admitted local clock");
    assert_eq!(initial.elapsed_since(initial), Some(Duration::ZERO));
    for _ in 0..200_000 {
        let current = clock.sample().expect("clock remains local");
        if current
            .elapsed_since(initial)
            .expect("same monotonic clock")
            > Duration::ZERO
        {
            assert_eq!(initial.elapsed_since(current), None);
            return;
        }
        spin_loop();
    }
    panic!("counter did not advance on the admitted Hart");
}

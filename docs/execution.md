# Kernel Unit execution

## Provisioning

On RV64, the trusted image consumes the Boot handoff to establish cooperative execution
on the Boot Hart. It supplies two resident 16 KiB stacks in the shared Bare
Kernel Nest. Each Unit reserves one stack and a generational identity.
`Execution::with_minor` accepts a resident `entry::boot::Minor` with its own
dispatcher stack. The platform can use `start_minor` to start
one selected Hart through OpenSBI HSM. Room admits the minor after its
trusted supervisor entry establishes its local execution state.
The 32 KiB minor dispatcher stack holds the live startup frames, the
platform entry and its dispatcher-side callees.

Trusted Unit entries must fit their stack and run in supervisor mode with
interrupts and extended registers disabled. `create` pins the Unit to its
creating Hart. `create_migratable` requires a trusted entry that can resume
on either Hart and yields without retaining Hart-local resources.

## Dispatch and settlement

Room admits a Unit and reserves an exact occupancy. Metal enters through the
Unit's stack and saves its integer continuation at the call boundary on Yield.
A normal return records the Unit's sole end value. The dispatcher resumes on
its own stack before Metal issues the leave receipt. Room consumes that
receipt to make the Unit ready again or release the admission. The shared
Room and Unit state shares a short FIFO lock to publish eligibility and
settle an exact occupancy. Terminal transitions can wait with zero ordinary
guards. A try acquisition leaves the turn to any queued waiter.
A yielded continuation becomes available to the
other Hart only after settlement. An Ending Unit keeps its binding until the
admission is released. Reaping the Ended Unit allows the slot and stack to be
reused with a new generation.

## Cancellation

A trusted platform can stop a dormant Unit before its first submission.
Room admission and the cancellation decision share one protected scope.
`finish_end` retires the unused continuation, and `reap` returns the
cancellation outcome and releases the slot.

This execution path covers cooperative supervisor Units on up to two
resident Harts. The [verification guide](../server/tests/README.md) describes
the RV64 target scenarios. Hart detachment, preemption, hardware translation,
user traps and calls require separate implementation and verification.

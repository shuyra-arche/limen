# Kernel Unit execution

## Provisioning

On RV64, the trusted image consumes the Boot handoff to establish cooperative execution
on the Boot Hart. It supplies two resident 16 KiB stacks in the shared Bare
Kernel Nest. Each Unit reserves one stack and a generational identity.
If `Execution::new` or `Execution::with_minor` rejects the environment before
establishment, `EstablishFailure` returns the original `Boot`, the exclusive
stack supply and the reason. The trusted image can correct the environment and
retry with those same inputs.
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
A normal return from the Unit's entry records its sole end value. The terminal
handoff returns to the dispatcher and the Unit's continuation is no longer
eligible to run. A successful Yield returns `Ok(())` when a later dispatch
resumes the Unit. Metal issues the leave receipt after the dispatcher resumes
on its own stack. `pending_leave` reports absence for a participating Hart
without a stopped occupancy, and an error for an unattached Hart. Room
consumes the receipt to make the Unit ready again or release the admission.
One MCS Turn protects the shared Room and Unit state at publication and
settlement. Execution uses a one-member plan, which queues in FIFO order
under the current Hart's local protection. Metal rejects a switch while
the Hart has active synchronization participation. The plan ends its
borrows, settles all queue nodes and restores the original interrupt state
before a continuation can be entered or handed off.
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

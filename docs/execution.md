# Kernel Unit execution

## Provisioning

The trusted image consumes the unique Boot handoff through
`basis::execution::Execution::new`, supplying resident Unit records, two
16 KiB stacks with saved continuations, and independent `room::Harts<N>`
participation and dispatcher-context storage. Unit owns each exclusive stack
lease until its continuation has retired. The current RV64 profile uses Bare
translation and integer continuations; the UEFI profiles retain their own
verified native save shapes.

Construction does not attach a Hart. If the environment or supply is rejected,
`EstablishFailure` returns Boot and all original supplies so the image can
retry. Every actual Hart uses `Execution::attach` after its trusted entry.
The trusted image starts other Harts directly through `entry::boot::Minor`;
startup failure and an unconfirmed wake remain Boot's responsibility. Room
publishes each permanent participation and its associated machine record in
one protected operation. Only the current Hart uses its scope-checked local
locator, without a second Room acquisition. Duplicate and exhausted
participation are rejected before publication. The 32 KiB minor dispatcher
stack holds live startup frames, the platform entry and dispatcher-side callees.

Trusted Unit entries must fit their stack and preserve the selected supervisor
execution profile. `create` pins the Unit to its creating Hart.
`create_migratable` requires a trusted entry that can resume on any attached
compatible Hart and yields without retaining Hart-local resources.

## Dispatch and settlement

Room's `submit` establishes one admission and returns its `AdmissionId`.
Ready and Hart indices locate that admission's authoritative placement. Each
dispatch establishes a fresh occupancy for the same admission, identifying
the Unit, Hart and original participation. Metal enters through the Unit's
stack and saves its integer continuation at the call boundary on Yield.
A normal return from the Unit's entry records its sole end value. The terminal
handoff returns to the dispatcher and the Unit's continuation is no longer
eligible to run. A successful Yield returns `Ok(())` when a later dispatch
resumes the Unit. Metal issues the leave receipt after the dispatcher resumes
on its own stack. `pending_leave` reports the stopped occupancy to its owning
dispatcher. Room matches the private Metal receipt to the exact current
occupancy before making the Unit ready again or releasing the admission.
One MCS Turn orders the shared changes to Room placement and Unit state
at publication and settlement. Metal rejects a switch while the Hart has
active synchronization participation. The Turn ends its
borrows, settles all queue nodes and restores the original interrupt state
before a continuation can be entered or handed off.
A yielded continuation becomes available to another compatible Hart only
after settlement. An Ending Unit keeps its binding until the
admission is released. `finish_end` clears and returns the stack before the
Ended result becomes visible. Its Unit identity and result persist until
`reap` releases the identity slot. A replacement can reuse an available
stack while a prior result remains observable.

## Cancellation

A trusted platform can stop a dormant Unit before its first submission.
Room admission and the cancellation decision share one protected scope.
`finish_end` retires the unused continuation, and `reap` returns the
cancellation outcome and releases the slot.

This execution path covers cooperative supervisor Units on an independently
provisioned, finite set of resident Harts. Participants do not detach or reuse
slots in this prefix. The [verification guide](../server/tests/README.md) describes
the RV64 target scenarios. Hart detachment, preemption, hardware translation,
user traps and calls require separate implementation and verification.

See [Room](https://github.com/shuyra-arche/document-limen/blob/d1d729b65f438e95dba525f0dfc59e8a816dcefc/server/basis/room/README.md)
and [cooperative switching](https://github.com/shuyra-arche/document-limen/blob/d1d729b65f438e95dba525f0dfc59e8a816dcefc/server/metal/execution/cooperative.md)
for the candidate Hart participation and broader contracts.

# Kernel Unit execution

## Provisioning

The trusted image transfers the admitted Major and Terminal from Boot to
`basis::execution::Execution::new`. It supplies resident Unit records and
two 16 KiB Unit stacks. Room holds participation in independent
`room::Participants<N>` storage. Processor holds the machine continuations and
dispatcher stacks in separately supplied `processor::treaty::Dispatchers`.
Unit owns each exclusive stack lease until its continuation has retired.
RV64 uses Bare translation and integer continuations. The UEFI profiles
validate their respective native save shapes before entry.

On rejection, `EstablishFailure` returns the original Major, Terminal and
storage for retry. The image uses `processor::treaty::Minor` to start secondary
processors. Processor retains a launch with uncertain delivery.

After trusted entry the image calls `Execution::run` with a resident
`RuntimeEntry`. Processor binds one machine record to the current Executor
and transfers to that record's 32 KiB dispatcher stack. The entry's input
stays in its own permanent storage. The image settles or transfers
obligations of the abandoned frames before switching. Any firmware borrows
or outstanding machine effects retain their own retirement conditions.
The callback calls `Execution::attach` from the dispatcher stack to publish
Room participation. Only then can Room give this Executor an occupancy.
Duplicate and exhausted stack requests fail before publishing a binding.

Trusted Unit entries must fit their stack and preserve the selected supervisor
execution profile. `create` pins the Unit to its creating processor.
`create_migratable` requires a trusted entry that can resume on any attached
compatible processor and yields without retaining processor-local resources.

## Dispatch and settlement

Room's `submit` establishes one admission and returns its `AdmissionId`.
Trusted entry can create and submit a Unit before any Executor attaches.
Ready retains its original affinity until a compatible participant is available.
Ready and participation indices locate that admission's authoritative placement. Each
dispatch establishes a fresh occupancy for the same admission, identifying
the Unit, processor and original participation. Metal enters through the Unit's
stack and saves its integer continuation at the call boundary on Yield.
A normal return from the Unit's entry records its sole end value. The terminal
handoff returns to the dispatcher and the Unit's continuation is no longer
eligible to run. A successful Yield returns `Ok(())` when a later dispatch
resumes the Unit. Metal issues the leave receipt after the dispatcher resumes
on its own stack. `pending_leave` reports the stopped occupancy to its owning
dispatcher. Room matches the private Metal receipt to the exact current
occupancy before making the Unit ready again or releasing the admission.
Unit and Room each own a Turn. Admission and settlement acquire both
in canonical order. Unit-only and Room-only operations acquire their own Turn. Metal rejects a switch while the processor has
active synchronization participation. The acquisition ends its
borrows, settles all queue nodes and restores the original interrupt state
before a continuation can be entered or handed off.
A yielded continuation becomes available to another compatible processor only
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
provisioned, finite set of Executors. Participants do not detach or reuse
slots in this prefix. The [verification guide](../server/tests/README.md) describes
the target scenarios. Detachment, preemption, hardware translation,
user traps and calls require separate implementation and verification.

See [Room](https://github.com/shuyra-arche/document-limen/blob/d1d729b65f438e95dba525f0dfc59e8a816dcefc/server/basis/room/README.md)
and [cooperative switching](https://github.com/shuyra-arche/document-limen/blob/d1d729b65f438e95dba525f0dfc59e8a816dcefc/server/metal/execution/cooperative.md)
for the participation and broader execution contracts.

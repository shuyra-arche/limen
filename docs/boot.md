# CPU boot

## Major handoff

The firmware or loader enters one major CPU. Metal selects a private 32 KiB
stack, masks interrupts and installs the architecture's early exception entry.
All three backends then call `entry::boot::start`.

Common major admission checks the hardware stack pointer against the reserved
range, reads back the installed CPU state and compares the CPU identity with
the supplied ID. The selected firmware adapter validates its input. The entry
calls the image-defined `limen_platform_ready(Boot)` once. The image receives the
major ID and the admitted device-tree extent or firmware map when available.
A rejected handoff uses early diagnostics and requests failure shutdown.
[Entry](../server/src/entry/boot/mod.rs) · [Admission](../server/src/metal/boot/major.rs)

Boot owns the major stack as an aligned Rust static. The image keeps
the object resident and writable for the early boot path. On RV64, OpenSBI
enters a single major hart. The entry loads the stack pointer and installs the
diagnostic vector, then clears BSS using registers alone before calling Rust.
HSM starts minors only after the major handoff. The stack resides in the
reserved image range. Any later return of early physical memory requires a
runtime stack handoff, departure of its old users and translation retirement.

## Minor launch

The image selects a known inactive CPU and provides a resident `Minor` record
with a private 32 KiB stack. The major writes the target ID, stack, entry and
argument, publishes the record, and asks the backend to wake the target. The
image, launch record and required mappings must stay resident.

The target observes publication and checks its identity before claiming the
record. On its own stack it verifies the CPU ID, stack pointer and exception
entry. Only then does it publish `Ready` and invoke the supplied entry.
`Minor::ready()` observes that transition.

A definitive rejection releases the record for retry. An unconfirmed xAPIC
delivery keeps the record, trampoline, GDT and mappings reserved because the
minor can still enter. [Launch state and safety contract](../server/src/metal/boot/minor.rs)

## Adding a backend

Use `metal/boot/<arch>/major.rs` and `minor.rs` for the CPU entry instructions.
Place each entry's instructions and operands in its Rust naked function.
Refer to Rust-owned entries and storage with `sym`, sizes and offsets with
`const`, and align the function when required by the CPU. Export a fixed name
when an external consumer requires it, as the RV64 ELF linker does for `_start`.

The files directly in `metal/boot/` own admission, launch state and early
diagnostics. Fixed stack storage and local CPU reads live in `metal/hart/`;
DTB and firmware protocols live in `metal/firmware/`. The checked bootstrap
cursor is under `metal/allocator/` and is not a runtime allocator. Each
function selects its own machine backend; Boot uses the firmware adapter
for the chosen entry protocol. The platform image supplies board discovery, device
addresses and startup resources. [Platform contracts](platforms.md) · [Early exceptions](exceptions.md)

## Mapped storage

A physical extent is a coordinate, not a Rust pointer or an access grant.
The actual mapping owner supplies an exclusive `access::MappedRange` with
valid storage and physical correspondence for the entire borrow.
`MappedRange::split` takes a strictly interior `ByteLength` and consumes the
original borrow to produce two disjoint borrows of the same backing storage.
An invalid offset returns the original borrow. Root reservation retains the
selected mapping and accounts for its whole reserved granules.

An owner starting with a raw pointer from the original identity mapping can
use `MappedRange::from_identity`. It checks both machine-width endpoints,
nonempty slice length and the base address before borrowing. It derives the
slice from the supplied pointer. The owner must already hold the complete
writable RAM range and preserve exclusive access, mapping and attributes
for each derived use.

For ordinary issued material, `allocator::MappedStorage::prepare` combines
the unique Allocation and its exclusive mapping. The complete physical
ranges must match; equal ranges or lengths alone do not prove the original
allocation generation or the real mapping. Its unsafe boundary requires
that independent evidence from the actual owner. A failed pair returns
both original inputs. Pool installation consumes the pair only after all
directory, layout, capacity and original Return checks succeed. It retains
the actual source and mapping lifetime, and does not treat software closure
or an address as proof of retirement. No arbitrary slice or implicit global
heap is accepted by the Pool.

The three boot fixtures use their actual reserved or issued RAM and the
original machine mapping. Each verifies native access at both ends of a
Pool group's original allocation and repeats the check after the same range
is issued with a new generation. Their evidence covers major-CPU data
access. Executable material, table publication, successor roots and
firmware CPU/DMA retirement have their own target checks.

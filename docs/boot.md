# CPU boot

## Major handoff

The firmware or loader enters one major CPU. Metal selects a private 32 KiB
stack, masks interrupts and installs the architecture's early exception entry.
All three backends then call `entry::boot::start`.

Generic major admission checks the hardware stack pointer against the reserved
range, reads back the installed CPU state and compares the CPU identity with
the supplied ID. The selected firmware adapter validates its input. The entry
calls the image-defined `limen_platform_ready(Boot)` once. The image receives the
major ID and the admitted device-tree extent or firmware map when available.
A rejected handoff uses early diagnostics and requests failure shutdown.
[Entry](../server/src/entry/boot/mod.rs) · [Admission](../server/src/metal/generic/boot/major.rs)

Generic Metal owns the major stack as an aligned Rust static. The image keeps
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
minor can still enter. [Launch state and safety contract](../server/src/metal/generic/boot/minor.rs)

## Adding a backend

Use `metal/<arch>/boot/major.rs` and `minor.rs` for the CPU entry instructions.
Place each entry's instructions and operands in its Rust naked function.
Refer to Rust-owned entries and storage with `sym`, sizes and offsets with
`const`, and align the function when required by the CPU. Export a fixed name
when an external consumer requires it, as the RV64 ELF linker does for `_start`.

The matching files in `metal/generic/boot/` own admission and launch state.
Shared stack storage, DTB header validation, diagnostic output, early service
publication and allocator code live in `metal/generic/`. Select the CPU
backend at the module boundary. The backend reads hardware state and selects
the firmware adapter. The platform image supplies board discovery, device
addresses and startup resources. [Platform contracts](platforms.md) · [Early exceptions](exceptions.md)

# CPU boot

## Major handoff

The firmware or loader enters one major CPU. The trusted image provides its
ordinary stack and local storage with a fixed Sync node supply. Metal enters
the supplied stack, masks interrupts and installs the architecture's terminal
vectors. All three backends then enter the shared Metal admission path.

Common major admission checks the hardware stack pointer against the reserved
range, reads back the installed CPU state and compares the CPU identity with
the supplied ID. The selected firmware adapter validates its input. The entry
calls the image-defined `limen_platform_ready(Boot)` once. The image receives the
major ID and the admitted device-tree extent or firmware map when available.
Metal validates the resident stack ranges against the image under the original
trusted identity mapping. It delivers a single owned Admission through the
Boot Treaty. Entry then calls the image; it uses the physical stack and
firmware-buffer ranges carried by admission. A rejected handoff uses early
diagnostics and requests failure shutdown.
[Entry](../server/src/entry/boot/mod.rs) · [Boot Treaty](../server/src/metal/platform/boot/treaty/mod.rs)

On RV64 the image links its materials with `limen_riscv_major!`. OpenSBI enters
one major Hart. The entry loads the linked stack pointer and installs the
diagnostic vector, then clears BSS using registers alone before calling Rust.
UEFI images pass their statically supplied `MajorMaterial` at handoff. Each
stack must be positively sized, 16-byte aligned and sufficient for the
trusted caller's actual frames. HSM starts minors only after the major handoff.
The stack resides in the
reserved image range. Any later return of early physical memory requires a
runtime stack handoff, departure of its old users and translation retirement.

Major material reserves one image-resident slot for the parsed DTB, CPU and
ACPI descriptions. The firmware adapter prepares it once. The full slot
must lie inside the retained image and outside both startup stacks.
Independent borrows keep the lifetime of the original descriptor bytes.
The UEFI ACPI reader is used during parsing and is gone before delivery.
The backing DTB, ACPI and memory-map ranges retain their own retirement
conditions.

## Minor launch

Through the Processor Treaty, the launch owner selects a known inactive CPU
and constructs a resident `Minor` with exclusive stack and local materials.
The major writes the target ID, stack, entry and argument, publishes the record,
and asks the backend to wake the target. The image, supplied materials, launch
record and required mappings must stay resident.

The target observes publication and checks its identity before claiming the
record. On its own stack it verifies the CPU ID, stack pointer and exception
entry. Only then does it publish `Ready` and invoke the supplied entry.
`Minor::ready()` observes that transition.

A definitive rejection releases the record for retry. An unconfirmed xAPIC
delivery keeps the record, trampoline, GDT and mappings reserved because the
minor can still enter. [Launch state and safety contract](../server/src/metal/processor/executor/launch/mod.rs)

## Adding a backend

Put major entry instructions in `metal/platform/boot/<arch>/major.rs` and the secondary launch instructions in `metal/processor/executor/launch/<arch>.rs`.
Place each entry's instructions and operands in its Rust naked function.
Refer to Rust-owned entries and storage with `sym`, sizes and offsets with
`const`, and align the function when required by the CPU. Export a fixed name
when an external consumer requires it, as the RV64 ELF linker does for `_start`.

Boot admits the initial firmware handoff. Processor's shared Executor entry
checks machine state for both startup paths. Processor owns the resident
secondary launch and the local execution environment. Execution Terminal
owns the terminating exception vectors and emergency stack. The handoff
carries the platform's terminal output and reset operations into Execution.
DTB and ACPI are owned by Platform Description.
SBI, PSCI and UEFI operations are owned by Platform Firmware. The platform
image supplies device addresses and startup resources.
[Platform contracts](platforms.md) · [Early exceptions](exceptions.md)

## Mapped storage

A physical extent is a coordinate, not a Rust pointer or an access grant.
The actual mapping owner supplies an exclusive `memory::access::treaty::ram::MappedRange` with
valid storage and physical correspondence for the entire borrow.
`MappedRange::split` takes a strictly interior `ByteLength` and consumes the
original borrow to produce two disjoint borrows of the same backing storage.
An invalid offset returns the original borrow. Root reservation retains the
selected mapping and accounts for its whole reserved granules.

Entry checks retirement evidence and organizes RAM before it hands Allocator
the permanent `RootStorage` and the resulting `RegionDescription` values.
Allocator validates the root layout, reserved supply and initial receivers
before Entry consumes the mapping. A delayed region is admitted through the
original owner's `Retirement::grant` and its unique allocator `Grant`.

Boot can supply a permanent `PendSpace::for_page_tables` spending position.
Its trusted issuer supplies the actual Allowance, request-size bound and
alignment bound. It accepts the PageTable purpose and records a read/write
ceiling for first access. The issued Allocation carries the original slot
through take and return. Cell publication must revalidate the live holding
and the actual receiver. Metadata, boot reservations and allocations
created by conversion have an empty first-access ceiling.

An Internal root's `TableStore` reserves its own stable identity before
native publication. It accepts the original mapped allocation and a
resident `CellSlot` jointly with the live page-table issuer. That commit
gives the private construction one typed Pend. The store consumes that
holding when the Cell's prepared access becomes accepted. The original
read/write ceiling covers initialization to invalid descriptors. A root
withdrawn in this private state returns its exact allocation through the
normal prepaid Allocator receipt. Native root installation requires a
separate effect and retirement contract.

An owner starting with a raw pointer from the original identity mapping can
use `MappedRange::from_identity`. It checks both machine-width endpoints,
nonempty slice length and the base address before borrowing. It derives the
slice from the supplied pointer. The owner must already hold the complete
writable RAM range and preserve exclusive access, mapping and attributes
for each derived use.

For ordinary issued material, `memory::allocator::treaty::MappedStorage::prepare` combines
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

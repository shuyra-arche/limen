# Limen tests

Host suites under `entry/boot/` and `metal/` exercise bounded Device Tree
traversal, original node scope, address translation, ACPI source and table
validation, UEFI descriptor geometry, SBI extension discovery, the bounded allocation
cursor, fixed identity slots and portable Arm CPU/firmware descriptions. The
RV64 PTE activity experiments under `metal/memory/mmu/riscv64/internal/activity/`
exercise an independent candidate atomic-word model, not a production table
or actual hardware A/D and translation retirement. The Identity
suite exercises the table implementation with owner-supplied slot storage,
distinct scopes and separate allocation and return tables. Its compiler tests
cover token ownership, table categories and the lifetime of a lookup borrow.
The kernel library runs owner-local unit tests from `tests/` and links
its public ACPI and device-tree contracts through the single ordinary Cargo
integration target `contracts`. The root `tests/contracts.rs` declares the
owner module tree; add public contract cases to their owner modules rather than
adding a Cargo target or editing the manifest. Private unit tests remain
declared by the actual production owner and live under `tests/`.
The Module suite retains a Host package for its separately linked ELF
fixtures. Identity’s compile-time cases retain a small compile fixture.
Run the commands from the repository root so Rustup selects the fixed
[toolchain](../../rust-toolchain.toml).

```sh
RUSTFLAGS=-Dwarnings cargo test --locked --workspace
cargo test --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
```

The workspace defaults to the native host target. Ordinary tests use Rust's
native harness; firmware and guest commands explicitly select their target.
Run a single integration domain with the native module-path filter:

```sh
cargo test --locked -p kernel --test contracts -- metal::firmware::acpi
cargo test --locked -p kernel --test contracts -- metal::firmware::devicetree
```

Keep independent build fixtures, compiler contracts and target guests separate
only when they require their own compilation or execution boundary.

## Metal profile boundaries

Host and target compile the same Clock identity, observations and deadline
rules, GIC private-input state, MMU installation record, synchronization
state, identity slots, allocator geometry and boot descriptions. A supplied
Clock observation cannot produce a deadline for an admitted native counter.
An ordinary Host lacks installed Hart protection and returns a context error
on lock acquisition. The owner-local Sync tests supply controlled observations
at the local-address and interrupt-mask edges.

| Remaining profile selection | Native dependency |
| --- | --- |
| `chrono/native` and the Arm, RV64 and x86 counter bindings | Trusted current-Hart observation, CNTVCT/CNTFRQ, `rdtime`, or CPUID/RDTSC. |
| Arm GIC CPU interface, distributor barrier, redistributor wake and private-input operations, and virtual timer | ICC and comparator registers, the current PE, ordered GIC register writes, and readback. |
| RV64 external barrier, supervisor reception, timer and SBI software delivery | I/O fence, supervisor CSRs, comparator access and the selected SBI call. |
| x86 local APIC native operations | Live local APIC mode, registers and current CPU observation. |
| CPU MMU native context bindings and RV64 encoder binding | Live translation controls and maintenance, or the resident module's native call ABI. Format, installation transitions and offer checks are shared. |
| SMMUv3, RV64 IOMMU and VT-d ordering constructors and operations | Native memory ordering, cache-line publication and register synchronization. The controller records and descriptor encoders are shared. |
| Platform boot major, minor, trap, services and ISA entry modules, and PSCI invocation | Installed machine entry, native stacks and vector ABI, terminal service delivery, or an actual PSCI conduit. SBI and UEFI handoff selection follows the firmware profile. |
| Hart ISA backends and local entry, stack access, resident storage installation and protection | Actual entry registers, trusted stack range, CPU-local address and interrupt masking/restoration. Resident construction requires native entry or the owner-local Host observation fixture. |

The Metal Sync Host suite executes the production MCS queue with the same
spin hint and atomic operations as the target. Tests arrange a real tail
exchange before linking, inspect the private departure decision, then finish
that decision; separate complete acquisitions verify linked FIFO handoff
and node reuse. Test coordination does not prove hardware wait bounds or
replace production ordering. Host Allocator tests use the
production Turn and Hart participation with a per-thread local address
and saved interrupt bit. The target tests cover machine entry and IRQ behavior. The compiler checks use the actual
target library to verify scoped single-lock and heterogeneous access. They
reject escaping or overlapping borrows, moving the access token out of the
callback, and crossing Hart boundaries with it.

```sh
bash server/tests/metal/sync/contracts.sh
```

The Allocator Host suite checks the fixed Buddy forest against an independent
flat occupancy model. It enumerates small physical regions and fragmented
states, exercises exact edits and bounded scan resumption, and checks the
structural visit bounds. The target boot suites exercise the earlier physical
handoff. Runtime admission of a Buddy region requires the Allocator source.

```sh
RUSTFLAGS=-Dwarnings cargo test --locked -p kernel --lib --target x86_64-unknown-linux-gnu metal::allocator
```

## Interrupt backends

| Target | Source and receiver | Observed behavior |
| --- | --- | --- |
| RV64 virt | 16550 UART and Goldfish RTC → PLIC supervisor contexts; Sstc → supervisor timer line | Two physical PLIC contexts admit independent routes. The boot Hart receives three interrupts and completes each original claim. Its two timer deadlines use the real STIMECMP after taking over the inherited comparator. |
| RV64 virt with AIA | 16550 UART → APLIC direct IDC; SBI TIME programs the supervisor comparator | The interrupt guest exercises direct delivery, deadline replacement and hardware shutdown. |
| x86_64 q35 | COM1 → I/O APIC → local xAPIC | The BSP receives a fixed vector through a guest IDT gate, checks the in-service vector, clears the device and sends the original EOI. |
| AArch64 virt | Two PL011 UARTs → GICv3 Distributor → Group 1 CPU interface | The guest verifies independent source bits, exact IAR/EOI pairing and the active bit before and after split deactivation. |

The RV64 and UEFI interrupt scenarios execute from the platform fixture’s
inherited mappings. Each guest owns its reception interval. Production Trap
dispatch and driver Sign delivery have separate entry and lifecycle contracts.

### APLIC and ACLINT

[The interrupt target](target/metal/interrupt/riscv64/aplic/) selects the exact single-Hart
QEMU machine profile `virt,aia=aplic,aclint=on` with `rv64,sstc=false`.
It exercises real supervisor timer and UART external traps, original claim
retention, explicit state admission, rearm, shutdown and rejected operations.
The guest temporarily installs its own bounded reception window; this is not
ordinary runtime Trap or Interrupt Server integration.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/metal/interrupt/riscv64/aplic --kernel "$PWD" \
  --output /tmp/limen-interrupt --profile debug
```

Repeat with `--profile release`. The [Quint state contracts](https://github.com/shuyra-arche/document-limen/tree/master/model/interrupt)
also cover unknown readbacks, retained pending edges during shutdown, and
finite identity exhaustion. Their abstract evidence inputs are not hardware
fault-injection tests.

### PLIC, Sstc, I/O APIC and GIC

Run a suite from the repository root with the same runner and a fresh output
directory. Each suite owns its manifest, hardware inputs and expected
observations.

| Scenario | Suite |
| --- | --- |
| RV64 PLIC with Sstc | [riscv64/plic-sstc](target/metal/interrupt/riscv64/plic-sstc/) |
| Local APIC startup failure | [x86_64/apic-failure](target/metal/interrupt/x86_64/apic-failure/) |
| q35 I/O APIC reception | [x86_64/ioapic](target/metal/interrupt/x86_64/ioapic/) |
| Arm GIC reception | [aarch64/gic](target/metal/interrupt/aarch64/gic/) |

## RV64 boot

The Boot owner keeps its platform image and assertions in [target/entry/boot/](target/entry/boot/).
The target runner archives the candidate Git revision. Commit changes before
starting verification so the report identifies the source it actually ran.
Set `QEMU_RISCV64` to QEMU 11.1.3 and `QEMU_OPENSBI` to OpenSBI firmware
with the debug console and system reset extensions. The cross-Hart scenario
also uses the hart state management extension.

```sh
bash tools/tests/discover.sh "$PWD" ""
cargo build --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/boot --kernel "$PWD" \
  --output /tmp/limen-boot --profile debug
```

At the firmware entry address the test image writes a nonzero BSS sentinel
and jumps to the production `_start` with the original firmware arguments.
The callback checks that the sentinel was cleared. The runner requires the
complete serial assertions, one QMP `SHUTDOWN` event with `guest=true` and
`reason=guest-shutdown`, and a clean QEMU exit. The serial log, QMP transcript
and verdict are preserved with the candidate revision.

The callback also checks that the production parser names the configured 128 MiB
RAM bank and retains the OpenSBI firmware reservation from the actual DTB.
The Boot Host suite exercises both DTB reservation representations and rejects
truncation, inaccessible extents, overflow, excess ranges and unresolved
dynamic reservations.

The Boot owner also exercises a terminal trap during the BSS clear. After a
normal boot-handoff run, `target/entry/boot/fault.sh` verifies the recorded ELF
hash and copies that ELF. It checks the actual instruction sequence and replaces
only the increment immediately following the first BSS store with an illegal
instruction. The unmodified ELF remains the normal-boot evidence. The fault
case requires the exact illegal-instruction PC, the interrupted major stack
pointer, a complete terminal diagnostic, and a native QMP guest-shutdown
with clean process exit. The pinned OpenSBI syscon-poweroff path does not
expose the SBI reset reason in QMP; the diagnostic and absence of the normal
handoff marker distinguish the injected failure from a normal boot. The test
does not independently observe the SBI reset reason.
Its image, injection record, serial output and verdict are kept beside the
normal result. No production hook or extra linker symbols are used. Run it
against the selected output plan with `PLAN` set to the discovery JSON:

```sh
bash server/tests/target/entry/boot/fault.sh "$runner" "$evidence"
```

The Target workflow runs affected owner-local `suite.json` scenarios with the declared machine and Hart count and uploads their evidence. Ordinary boots enter on the
firmware-selected CPU. The RV64 cross-Hart scenario provisions three
participation slots and checks rejection of a fourth actual Hart; both UEFI
board fixtures exercise two participants.

Target discovery compares the PR against its merge base and selects each board and
profile from the owner manifests. Changes to a board backend or its fixture select
that board. Shared boot entry, verification runner, Rust and QEMU version changes select
all affected guests. Manual dispatch runs the full suite. One Target job installs
the selected emulators, firmware and Rust targets, builds the host runner once,
and executes up to two guests concurrently. The artifact contains a per-case log,
evidence directory and exit status, plus the selected plan and combined results.

Hardware execution and later runtime contracts require separate verification.
The Host Hart protection checks include the common cooperative-switch gate;
RV64 machine switching is verified by the single- and dual-Hart target guests.
ACPI parsing and retained exclusions are compiled independently of ISA; the
OpenSBI boot guest checks that ACPI is absent from its actual handoff.

## RV64 Kernel Unit execution

Run [target/entry/execution/](target/entry/execution/) with the same runner and
firmware. Use a fresh output path for each profile.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/execution --kernel "$PWD" \
  --output /tmp/limen-execution --profile debug
```

The guest verifies separate resident Unit stacks in RV64 Bare mode, Yield and resume,
exact leave receipts, ending and reaping, and reuse with a new identity. It
also exercises rejection before occupancy, Busy, stale identity and dormant
cancellation. A rejected execution constructor returns the original boot handoff
and exclusive stack supply, which the guest uses to retry successfully. A
cancelled Unit releases its slot under a new generation
without executing its entry.

Run [target/entry/execution-harts/](target/entry/execution-harts/) to exercise
two-Hart migration and a three-participant capacity boundary on a four-Hart
QEMU machine.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/execution-harts --kernel "$PWD" \
  --output /tmp/limen-execution-harts --profile debug
```

The guest starts another Hart through OpenSBI and coordinates the two Harts
through real Turn contention and handoff. After migration, a third Hart attaches
and runs a Unit using the existing stack supply; a fourth reaches its trusted
entry but cannot attach or dispatch through the exhausted Room. It forces Busy after each allowed
nonempty Fast prefix, checks that unused callback captures can reacquire a
released lock, and checks Slow acquisition across opposite insertion orders.
It also verifies rejection of Yield while the Hart participates in a Turn.
The idle minor skips
a Ready Unit pinned to the major. It then verifies independent occupancies,
rejection before the original leave is settled, continuation migration after
settlement, and rejection of a late original occupancy after re-entry. A
test-only linker wrapper sets distinct `s0`–`s11` values for each saved
frame and checks their restoration at the production switch boundary,
including the call that resumes on the other Hart.

## q35 Local APIC timer

The [one-shot timer guest](target/metal/interrupt/x86_64/apic-timer/) settles the
inherited legacy countdown on q35, then arms the original local timer, observes its leased vector through a temporary
test IDT, and stops the source. The vector stays occupied until the original
Local APIC in-service entry has been completed. It checks that the vector can
be returned after EOI and that the stopped countdown emits no second entry.

## UEFI Kernel Unit execution

Each UEFI scenario builds the guest next to its own `suite.json`. Its Cargo
package owns both firmware entry and the kernel handoff. The guest calls the
shared `uefi-fixture` library to prepare the board and retain the firmware
inputs, then runs its own assertions. The target workspace shares a lockfile
across these guests, and the launcher uses the suite's board configuration
and evidence expectations to build and observe the selected guest.

The [platform execution suite](target/basis/execution/platforms/) boots q35
or Arm virt, attaches both entered Harts to the actual Room and runs a
migratable Kernel Unit. It verifies both migrations, address stability on the
Unit stack, preserved control and callee-saved registers, cancellation,
stale departure rejection and stack reuse. Select a board with `--board`,
or omit it to run both.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/basis/execution/platforms --kernel "$PWD" \
  --output /tmp/limen-platform-execution --profile debug
```

## q35 and AArch64 virt boot

All target suites use the exact upstream version in [qemu.version](qemu.version).
The Target workflow selects the Debian trixie-backports snapshot providing
11.1.3. Install its x86, Arm and RISC-V system emulators, plus `ovmf`, `qemu-efi-aarch64`,
`opensbi` and `jq`. By default the platform suite reads q35 firmware from
`/usr/share/OVMF` and virt firmware from `/usr/share/AAVMF`. Override these
directories with `OVMF_DIR` and `AAVMF_DIR` respectively.

```sh
rustup target add x86_64-unknown-uefi aarch64-unknown-uefi
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/platforms --kernel "$PWD" \
  --output /tmp/limen-platforms --profile debug
```

Both board fixtures receive the same `limen_platform_ready(Boot)` call. The
major requests a minor identified by UEFI MP Services on q35 or the firmware
DTB on Arm. The loader prepares the startup page and delay calibration on q35,
or reads the PSCI conduit and serial device on Arm. The minor checks its own CPU ID and stack
before reporting back. The major records both observed IDs and verifies that
a second start request is rejected. Both board fixtures emit their assertions and terminate through the shared
boot console and shutdown API. The runner boots both boards from the candidate Git archive. Pass `--board q35`
or `--board virt` to run just one. Use `--profile
release` with a fresh output directory for optimized images. q35 requires
the exact debug-console assertions and the guest's debug-exit status 33.
Arm virt requires serial assertions, a QMP guest-shutdown event and exit 0.
Each board retains its boot image, input hashes, QEMU version, guest log,
QMP transcript and exit status. Both test images use the same UEFI entry and
retain the final memory map after exiting Boot Services. Arm checks the
configured 256 MiB RAM bank and verifies that the DTB and the UEFI map
corroborate the reported whole-page ranges, excluding the retained DTB and
firmware reservations. q35 checks the ACPI-only handoff. The q35 guest requires
enabled MADT entries for the major and selected minor, the local xAPIC
cross-check in Boot, I/O APIC and source overrides, and a physical exclusion
for each admitted table. Arm checks the source-scoped GICv3 windows and
timer PPI, compares the major's affinity and the target with PSCI and
verifies the accessible counter frequency. The Boot Host
suite covers overlapping reservations, partial pages, absent coverage and
conflicting UEFI types and runtime attributes.
[Platform contracts](../../docs/platforms.md)

The three board fixtures also check the real image, major and trap stacks,
and the retained firmware inputs that their boot paths supply. Each checks
that missing retirement evidence retains a RAM range. Host cases exercise
overlapping claims, partial-page exclusions and the four independent
retirement conditions for a checked physical scope.
The target fixtures also check zero organized regions and zero Bump roots
when their actual handoff has no retirement or mapping evidence. Host checks
exercise sorted and attributed regions, granule exclusions, capacity failure,
and the reservation of real backing bytes supplied by a Host fixture.

On bare RV64 virt, the boot guest also hands over a bounded RAM extent
with a real mapping. It verifies the fixed root exclusion, the first
issued allocation, source-wide negative search, delayed admission and
return through the original records. The UEFI guests exercise the same
entry with absent retirement and mapping evidence and require that it
leave the source unpublished. The Host boot suite additionally exercises
joint lock contention, cancellation, allowance reuse and independent
return history.

## Native translation

The paging and IOMMU suites own their test conditions. They reuse the platform
fixture for firmware inputs, mappings and shutdown. The IOMMU scenarios use
the same EDU transfer and completion protocol through the board’s PCI fixture.

| Scenario | Suite |
| --- | --- |
| x86-64 CPU paging | [x86_64/paging](target/metal/memory/mmu/x86_64/paging/) |
| AArch64 CPU paging | [aarch64/paging](target/metal/memory/mmu/aarch64/paging/) |
| RV64 CPU paging and module-bound Sv39/Sv48/Sv57 | [entry/module-riscv](target/entry/module-riscv/) |
| RISC-V IOMMU | [riscv64/iommu](target/metal/memory/mmu/riscv64/iommu/) |
| Intel VT-d | [x86_64/vtd](target/metal/memory/mmu/x86_64/vtd/) |
| AMD-Vi | [x86_64/amdvi](target/metal/memory/mmu/x86_64/amdvi/) |
| Arm SMMUv3 | [aarch64/smmu](target/metal/memory/mmu/aarch64/smmu/) |

The CPU paging guests use the same local context consumer. Native composition
supplies the retained table trees and checked control profile, then the consumer
verifies the exact original and installed contexts and restores the original
context after exercising real translations. RV64 also uses this path for the
module-produced successor context on both Harts, with fetch synchronization
performed by the module fixture. The local readback records the installed
context. The table and residency owners still need their own retirement evidence.

## Module loading

The Host module suite generates independent ELF64 images for RV64, x86_64
and AArch64 using the pinned compiler and linker. It checks bounded format
admission, candidate selection, file-backed entries, R/RX/RW layout, BSS
zeroing and relative relocation into private mapped storage. It also
checks exact provider identity, dependency retention, mismatched RV64
encoder dependencies and ABI, admission racing with Shut, and material
retention on failed publication and retirement.

```sh
RUSTFLAGS=-Dwarnings cargo test --locked -p kernel-host-module --target x86_64-unknown-linux-gnu
```

The [UEFI module guest](target/entry/module/) runs on x86_64 q35 with OVMF
strict-NX and on AArch64 virt with AAVMF strict-NX. Each guest uses one Hart,
prepares an independent module in private RW/NX pages, closes write access
before enabling execution, and checks effective permissions and aliases in
the active root. After ExitBootServices and the Limen major handoff, it
rechecks the retained mapping and calls the module's MMU geometry entry
through a retained use. The guest closes admission, drains two uses and
retains the mapped pages because the inherited root cannot be withdrawn
by this guest.

The [RV64 module guest](target/entry/module-riscv/) starts two Harts through
Limen on OpenSBI virt and checks the firmware descriptions of both Harts.
For each of Sv39, Sv48 and Sv57, a private root first verifies a high-half
load at every supported leaf level. Then the major prepares two independent
ELF images and revokes the staging writes before either becomes executable.
The selected encoder calls its retained geometry provider to produce page
table entries and the SATP context of a separate successor root. Both Harts
switch with local translation and instruction synchronization. Once firmware
confirms that the minor has stopped, the major withdraws both images in
dependency order, clears the inactive root and reuses the returned code pages
as RW/NX. Both roots are cleared before either table bank is reused.

Each guest requires the entry to return 4096 and QEMU to report a successful
exit or guest shutdown. Run the suites in either profile. The UEFI suite
accepts `q35` or `virt` for `--board`.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
    --suite server/tests/target/entry/module --kernel "$PWD" \
    --board q35 --profile debug --output /tmp/limen-module
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
    --suite server/tests/target/entry/module-riscv --kernel "$PWD" \
    --profile debug --output /tmp/limen-riscv-module
```

## Foundation acceptance

RV64 virt, x86_64 q35 and AArch64 virt run the same foundation assertions in
Debug and Release. Each source covers six checked RAM pages. The UEFI fixtures
take and release a private unused grant from the firmware map before
ExitBootServices. After exit, they require the entire grant to remain
conventional and disjoint from every boot exclusion. AArch64 also requires
DTB corroboration. The major retains the original identity view and the
fixtures leave all firmware buffers and tables outside the grant. Before
recording its final scoped condition, each UEFI guest checks that incomplete
evidence still prevents RAM admission.

| Scenario | Required observation |
| --- | --- |
| Root preflight | An invalid capacity leaves the mapping, region description and receiver fields intact. Successful publication accounts for all root and managed bytes. |
| Fragmentation and active identities | Two free single-page runs cannot satisfy a contiguous two-page request. An available page cannot be reserved when both Allocation slots are occupied, and the charge is unchanged. |
| Full typed Pool | A detached stable object stays readable through its counted reference. Reclamation is denied until the last reference ends. The existing reclaim and release path can free a slot while all other slots are held. |
| Original mapped storage | The guest checks identity correspondence and performs native volatile writes and reads at both ends of the exclusive Pool allocation. After retirement and reissue, a fresh borrow of the same range passes with distinct byte values. |
| Pending return and full history | Recovery finds the original return by scanning the bounded source. The retained result blocks a new reservation even after physical refund until the owner acknowledges it. The Pool separately demonstrates recovery from its own directory. |
| Same-address reuse and closure | The replacement receives a new generation and is taken and accessed after spending revocation. Old receipts cannot charge or refund it. Closing the Allowance moves the subsequent refund to the original Region and preserves the permanent PendSpace charge. |

The Boot Host suite checks `Busy` on the production Pend reserve plan and
verifies unchanged accounting before retry. The Identity Host suite checks
the maximum-generation boundary. The RV64 cross-Hart execution suite checks
joint Turn acquisition, node capacity and reuse, and rejection of a yield
with active protection. These are separate from the target foundation runs.
The boot scenarios cover their own RAM windows and do not exercise retiring
firmware-owned translations or device DMA, switching to a successor Kernel
Nest, or the full Kernel/User handoff and Seal.

The Treaty Model tests cover checked byte-coordinate kinds, marker-independent
value semantics, byte/page range lengths, overflow and compile-time rejection
of mixed coordinates or measures. Run `bash treaty/tests/compiler/check.sh`
to verify the standalone compiler contracts without enabling source doctests.
The existing firmware and module contracts
also verify nested bus translation, typed placements and unchanged storage
on rejected placement. RV64 mode tests check complete canonical intervals,
including the low-half exclusive endpoint and the noncanonical hole.

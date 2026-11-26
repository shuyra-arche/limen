# Limen tests

Host suites under `entry/boot/` and `metal/` exercise bounded Device Tree
traversal, original node scope, address translation, ACPI source and table
validation, UEFI descriptor geometry, SBI extension discovery, the bounded allocation
cursor, fixed identity slots and RV64 page-table entry operations. The Identity
suite exercises the table implementation with owner-supplied slot storage,
distinct scopes and separate allocation and return tables. Its compiler tests
cover token ownership, table categories and the lifetime of a lookup borrow.
Each suite keeps its own
`host/Cargo.toml`. The root workspace discovers these packages. Run the commands
from the repository root so Rustup selects the fixed [toolchain](../../rust-toolchain.toml).

```sh
RUSTFLAGS=-Dwarnings cargo test --locked --workspace --target x86_64-unknown-linux-gnu
cargo test --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
```

The Metal Sync Host suite exercises the production MCS queue with controlled
publication and release interleavings. The compiler checks use the actual
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
RUSTFLAGS=-Dwarnings cargo test --locked -p kernel-host-allocator --target x86_64-unknown-linux-gnu
```

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

The Target workflow runs affected owner-local `suite.json` scenarios on four virtual CPUs and uploads their evidence. Ordinary boots enter on the
firmware-selected CPU. The RV64 cross-Hart scenario and both UEFI board fixtures start a second participant.

Target discovery compares the PR against its merge base and selects each board and
profile from the owner manifests. Changes to a board backend or its fixture select
that board. Shared boot entry, verification runner, Rust and QEMU version changes select
all affected guests. Manual dispatch runs the full suite. One Target job installs
the selected emulators, firmware and Rust targets, builds the host runner once,
and executes up to two guests concurrently. The artifact contains a per-case log,
evidence directory and exit status, plus the selected plan and combined results.

Hardware execution and later runtime contracts require separate verification.

## RV64 Kernel Unit execution

Run [target/entry/execution/](target/entry/execution/) with the same runner and
firmware. Use a fresh output path for each profile.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/execution --kernel "$PWD" \
  --output /tmp/limen-execution --profile debug
```

The guest verifies separate stacks in the shared Kernel Nest, Yield and resume,
exact leave receipts, ending and reaping, and reuse with a new identity. It
also exercises rejection before occupancy, Busy, stale identity and dormant
cancellation. A rejected execution constructor returns the original boot handoff
and exclusive stack supply, which the guest uses to retry successfully. A
cancelled Unit releases its slot under a new generation
without executing its entry.

Run [target/entry/execution-harts/](target/entry/execution-harts/) to exercise
two participating Harts on a four-Hart QEMU machine.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/execution-harts --kernel "$PWD" \
  --output /tmp/limen-execution-harts --profile debug
```

The guest starts the minor through OpenSBI and coordinates the two Harts
through real Turn contention and handoff. It forces Busy after each allowed
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

## Module loading

The Host module suite generates independent ELF64 images for RV64, x86_64
and AArch64 using the pinned compiler and linker. It checks bounded format
admission, candidate selection, file-backed entries, R/RX/RW layout, BSS
zeroing and relative relocation into private mapped storage.

```sh
RUSTFLAGS=-Dwarnings cargo test --locked -p kernel-host-module --target x86_64-unknown-linux-gnu
```

The [q35 module guest](target/entry/module/) uses the Debian OVMF strict-NX
variant with one Hart. It prepares an independent x86_64 module in private
RW/NX pages, closes write access before enabling execution, and checks
effective permissions and aliases in the active root. After EBS and the
normal Limen major handoff, it rechecks the retained mapping and invokes the
module's MMU geometry entry using SysV64. The result must be 4096 and QEMU
must exit with the success status. Run the guest through the owner-local
runner with either profile.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
    --suite server/tests/target/entry/module --kernel "$PWD" \
    --board q35 --profile debug --output /tmp/limen-module
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

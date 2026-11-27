# Early exceptions

## Claiming the emergency stack

Processor Terminal reserves one 16 KiB stack for terminal diagnostics. Its atomic
pointer initially holds the first address past the stack, relocated with the
image. The stack is armed before RV64 clears BSS and before either UEFI image
installs its vectors. The aligned Rust stack object uses ordinary writable
image storage. On RV64 the initialized claim pointer lies outside BSS. An
exception during the clear abandons that clear on the sole major hart, while
other harts remain under firmware control. The reporter uses stack storage
without relying on its previous contents.

Each entry exchanges the pointer for null using registers. Only the CPU that
obtains a non-null top switches to it and enters Rust. A nested or concurrent
exception parks without touching the reporting CPU's stack. The terminal path
never returns the pointer. [Stack and reporting policy](../server/src/metal/processor/execution/terminal/trap.rs)

## Capturing the interrupted context

| Backend | Before switching stacks | On the emergency stack |
| --- | --- | --- |
| RV64 | `sscratch` points to the atomic slot. Take the top and preserve the interrupted SP. | Read `scause`, `sepc`, `stval` and `sstatus`. |
| AArch64 | Preserve the vector slot and the entry value of `SP_EL1`. | Read `ELR_EL1`, select the interrupted SP from the slot and interpret ESR and FAR when applicable. |
| x86_64 | Normalize the hardware error-code slot and preserve the frame address. | Decode the vector, error code, RIP and interrupted RSP. Read CR2 for a page fault. |

On x86 the CPU must be able to push its interrupt frame before the vector can
take the emergency stack. All three backends deliver the same five fields to
the reporter: cause, PC, value, interrupted SP and native status.

## Failure shutdown

The reporter attempts diagnostic output and requests failure shutdown even if
the writer refuses a byte. It parks the CPU if reset is unavailable or returns.
UEFI images supply immutable callbacks valid after Boot Services exit. An RV64
early trap probes SBI directly because it can precede normal service publication.
[Terminal](../server/src/metal/processor/execution/terminal/mod.rs)

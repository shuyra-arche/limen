# Limen implementation guide

| Topic | What it covers |
| --- | --- |
| [CPU boot](boot.md) | Major admission, minor launch records, readiness and retry |
| [Platform contracts](platforms.md) | What the OpenSBI and UEFI images supply and what each backend establishes |
| [Early exceptions](exceptions.md) | Emergency stack ownership, saved CPU state and failure shutdown |
| [Kernel Unit execution](execution.md) | RV64 dispatch, yield, migration, cancellation and reclamation |
| [Verification](../server/tests/README.md) | Host checks, QEMU guests and their expected observations |
| [Source history](history.md) | Upstream checkpoints and notices |

Metal separates Boot, Firmware, Hart, Sync and Execution by mechanism. The boot
handoff can establish an Allocator from admitted RAM and an exclusive mapped
root. Allocator owns exact physical allocation and return, while Internal MMU
validates mapped storage and its Host tests exercise the RV64 page-table entry
algorithms. See the [formal Metal organization](https://github.com/shuyra-arche/document-limen/blob/c6911e2f09384c9473467472edec2a7e6bfc3692/server/metal/README.md).

Module reads bounded ELF64 `ET_DYN` program headers and a source-scoped
`PT_NOTE` declaration to select a unique interface. Preparation copies and
zeroes private mapped storage and applies the target's relative relocations.
The returned layout gives numeric addresses and requested page permissions.
The initial mapping owner establishes actual permissions and instruction
visibility, then publishes into fixed boot slots. A published module owns
its backing and the uses of already published providers. Each consumer
keeps an exact ModuleUse through its calls and cleanup. Shut closes new uses.
The mapping owner settles old mapping and instruction effects before
reclaim returns the original backing. The RV64 Sv39Root v1 interface binds
the retained MmuGeometry v1 provider. It encodes branches, base-page leaves
and a SATP context using a synchronous call through that provider's versioned
import table. The mapping owner checks the returned encodings, supplies the
page tables and installs the verified root. See the [formal Module contract](https://github.com/shuyra-arche/document-limen/blob/c6911e2f09384c9473467472edec2a7e6bfc3692/server/metal/module/README.md).

For build commands see the [repository README](../README.md). The architecture
and Treaty contracts are maintained in [Document Limen](https://github.com/shuyra-arche/document-limen).

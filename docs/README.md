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

For build commands see the [repository README](../README.md). The architecture
and Treaty contracts are maintained in [Document Limen](https://github.com/shuyra-arche/document-limen).

# Limen implementation guide

| Topic | What it covers |
| --- | --- |
| [CPU boot](boot.md) | Major admission, minor launch records, readiness and retry |
| [Platform contracts](platforms.md) | What the OpenSBI and UEFI images supply and what each backend establishes |
| [Early exceptions](exceptions.md) | Emergency stack ownership, saved CPU state and failure shutdown |
| [Kernel Unit execution](execution.md) | RV64 dispatch, yield, migration, cancellation and reclamation |
| [Verification](../server/tests/README.md) | Host checks, QEMU guests and their expected observations |
| [Source history](history.md) | Upstream checkpoints and notices |

Metal follows actual mechanisms: Boot, Firmware, Hart and Execution are
separate owners with local machine backends. The checked bootstrap cursor and
RV64 page-table entry algorithms are tested at their Allocator and Internal MMU
locations; this layout change does not yet enable ordinary allocation or MMU
operations. See the [formal Metal organization](https://github.com/shuyra-arche/document-limen/blob/246170c626ee2b5e316753e95f50f1669e8f47dd/server/metal/README.md).

For build commands see the [repository README](../README.md). The architecture
and Treaty contracts are maintained in [Document Limen](https://github.com/shuyra-arche/document-limen).

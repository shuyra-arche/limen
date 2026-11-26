# Source history

Kernel and its previous API derive from [anreonyr/sqware](https://github.com/anreonyr/sqware)
at `672d8d2deafc8067c94b6af340a5f89c6a765c9c`. Historical Client mechanisms
derive from [Execution](https://github.com/shuyra-arche/execution) at
`1b333a28bc4c34beaea3f7986a9bb6e8ab2db414`. The internal SBI module derives
from [SBI](https://github.com/shuyra-arche/sbi) at
`2dc10bafb919eb2e8f6ca37c95cfd56f9a05831d`, originally extracted from sqware's
`crates/sbi` at `c970afaf7dae1145633278253351be729b625717`.
Git history preserves original authorship and migration records. These
checkpoints and this repository provide no license grant. Preserve
third-party notices with their code.

## Migration reuse

The foundation audit compares the [migration baseline][migration] with the
[Metal and Results contracts][specification], using the [previous implementation][previous]
for source and regression references. Review both the mechanism and its caller
before reusing code. A source routine that passes Host tests still needs the
evidence required at its eventual hardware or ownership boundary.

| Area | Reuse boundary |
| --- | --- |
| Boot and Firmware | Keep the current boot handoff, checked DTB extent, early stack and diagnostic mechanisms. The admitted firmware facts still require RAM classification, reservation exclusions and proof of retirement before they can supply the Allocator. |
| Identity | Reuse the checked generation increment pattern with the new table and its actual scope. The previous `basis/name` registry grows a hash index, its recycler allocates a bitmap on first use, and its serial has separate nonreusing semantics. The new table owns fixed slots, reservation, publication and exact reclamation. |
| Hart and Sync | Keep the real architecture localization and stack operations. The previous `PerHart` also embeds scheduler and MMU state. Keep its register and layout references where applicable. The migration baseline used a ticket lock. Document Limen [#89](https://github.com/shuyra-arche/document-limen/pull/89) specifies MCS and explicit Hart-local participation for the replacement. |
| Allocator | Keep checked alignment and endpoint arithmetic from the current bootstrap cursor. The previous Buddy's absolute page geometry is a reference for root decomposition. Its growable metadata, in-frame free links and enclosing Frame lock do not fit the fixed Region-local tree, source accounting and return contract. |
| Execution and MMU | Keep the verified RV64 cooperative switch and the existing PTE encoding and atomic update primitives. `Stacks` is the trusted platform's two-Unit supply in Bare mode. The PTE fixtures exercise software operations and A/D races on the Host. Full Unit storage, execution publication, translation retirement and reuse require their own evidence. |

[migration]: https://github.com/shuyra-arche/limen/tree/ad39080b618951cbca2e407c655456699a2e727e
[previous]: https://github.com/shuyra-arche/limen/tree/4640d13ed35b3166093cfadd1ac3634f2e7a3bc7
[specification]: https://github.com/shuyra-arche/document-limen/tree/246170c626ee2b5e316753e95f50f1669e8f47dd

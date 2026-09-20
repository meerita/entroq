---
title: Entroq Documentation
description: The documentation trees of Entroq, and which pages exist at this revision.
class: reference
audience: someone looking for a page of Entroq documentation
order: 1
---

# Entroq Documentation

This tree holds the user guide, the technical documentation, and the benchmark
documentation.

A page here states what Entroq does now. A page that would state intent is not written.

## Pages that exist

| Page | Class | Covers |
|---|---|---|
| [benchmarks.md](benchmarks.md) | reference | the measurement laboratory, the corpus, the metric set, the tiers, the measurement of Entroq beside the pinned competitors, and what a number from each tier licenses |
| [format.md](format.md) | reference | the frame, region, and block structure of a stream, the compressed block and the symbol model it carries, and what a conforming decoder accepts, refuses, and ignores |

## Pages that do not exist yet

Entroq admits no mode and ships no command line tool. `benchmarks.md` carries its numbers with
the scope they were measured under, and none of them has left a publication-tier record. The
pages below state behavior that does not exist, so they are named here and not written.

| Page | Will cover | Waits for |
|---|---|---|
| `architecture.md` | the model behind the codec | an admitted mode, and the measurements that admitted it |
| `streaming.md` | the streaming execution model and its bounds | a measured bound for each mode |
| `memory.md` | the memory bound each mode declares | a mode |
| `security.md` | behavior on malformed input, and the decoder resource policy | a decoder whose hardening has been campaigned, not sampled |
| `compatibility.md` | format versions, feature bits, and encoder generations | a frozen format version |
| `guide/` | tasks that reach a working result | something to compress |
| `api/` | the public API surface | an exported API |
| `cli/` | the command line surface | a command line tool |

## Contributing

`CONTRIBUTING.md`, at the repository root, states how to build a change, validate it, and
submit it. It is the contribution page of this project, and this tree does not repeat it.

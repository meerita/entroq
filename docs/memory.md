---
title: Memory
description: The memory each Entroq mode declares, what the figures cover, and how they are measured.
class: explanation
audience: someone sizing a buffer for Entroq, or auditing what it holds
order: 4
version_axes: [encoder_version]
encoder_version: 0.1.0. FAST is the single-table greedy path and BALANCED is the bounded chain at depth 32 behind the length-lazy parse.
---

# Memory

Entroq holds a bounded amount of memory in every mode. Each mode declares its bound before it
runs, and the test suite asserts the declaration instead of assuming it.

All figures are requested bytes counted from construction capacities and allocation sites.
They are not resident pages, and the allocator may round them. The input fragment a caller
passes in and the output fragment it takes back are caller owned and stay outside every
figure below.

## What the two figures mean

```text
steady state   what a machine holds between calls
peak           the most it holds at any instant, which adds what one block
               allocates and frees inside a call
```

A bound that counted only the steady state is a bound the codec exceeds every time it codes
a block. Both figures are declared for the encoder. The decoder declares its steady state;
its block transient scales with the admitted block size and is measured, not declared.

Nothing here scales with the total logical input size. Allocation happens at construction,
buffers are reused across blocks and regions, and a multi-terabyte logical stream passes
through the same storage. The growth curves below are the evidence.

## Encoder

Figures hold under the default layout of one region of 1 048 576 input bytes cut into blocks
of 65 536 input bytes, and under the conservative policy with a table ceiling of 65 536
bytes. A smaller block, a smaller region, or a lower ceiling lowers every figure that names
it; the declarations are functions of those three, not constants.

### Parse state, per block size

The parser owns its matcher, its window, and its sequence storage, allocated once when the
parser is built.

```text
FAST        matcher 65 536 + window 196 608 + sequences 65 536 + 16 384 steps
            steady state 262 144, declared 589 840, 4 allocations at setup
BALANCED    matcher 327 680 + window 196 608 + sequences 65 536 + 16 385 steps
            steady state 524 288, declared 851 984, 5 allocations at setup
```

The step counts are the block's worst case of one step per shortest match. The allocator
measurement holds exactly the declared figure on every input class, in exactly the setup
count, and the same figure from 1 MiB through 16 MiB of logical input.

### Block transient, per block size and table ceiling

Assembling one block allocates in proportion to that block and frees it before the call
returns. Each term names the production site it covers:

```text
symbol vectors    2 bytes per literal and per step symbol, the capacities the streams size
suffix buffers    3 streams, at most 16 suffix bits per symbol, doubled for growth
histograms        one 64-bit count per alphabet symbol, sized exactly
descriptions      one table per stream, at most 2 bytes per symbol of the widest span,
                  doubled for growth
payloads          fresh and one trial, at most 2 bytes per coded symbol plus the per-stream
                  flush, doubled for growth
tables            fresh tables inside the four policy shares, plus the held clones the reuse
                  decision carries beside them
scratch           16 384 bytes over the alphabet-scale build sites
```

At 65 536 input bytes under a 65 536 byte ceiling the terms are 229 382, 196 628, 3 240,
4 096, 721 068, 81 920, and 16 384, for a transient of 1 252 718 bytes. The formula is mode
independent: both modes assemble through the same machinery.

### Streaming encoder, default layout

One region of staged input, the blocks that region assembles to, the emitter steady state,
and one header scratch make the steady state. The peak adds the block transient.

```text
FAST        steady state 2 818 164, declared peak 4 070 882, measured peak at most 2 947 622
BALANCED    steady state 3 080 308, declared peak 4 333 026, measured peak at most 3 205 167
```

The measured maxima run over zeros, one-byte, incompressible, repetitive, text, and
near-window inputs. The BALANCED steady state passes the FAST one by exactly the parser
declaration gap of 262 144 bytes. The steady-state figure is constant from 1 MiB through
16 MiB of logical input in both modes, and the measured peak moves less than one block
length across a sixteenfold input range.

Construction allocates 7 times under FAST and 8 under BALANCED: the parser buffers, the
payload scratch, and the two staging buffers. An incompressible run of sixteen blocks
allocates exactly that count, which is what proves the steady state performs no per-block
allocation. Tables adopted per compressed block sit inside the ceiling the steady state
already counts.

## Decoder

The decoder holds a header scratch, room for one compressed block, one window of the bytes
its region produced, and the tables in force counted at the table memory the policy admits
rather than at whatever the last block left. Under the conservative policy the steady state
is 327 716 bytes. The decoder never learns the encoder's mode, so the figure is the same
for FAST and BALANCED streams.

The decode transient of symbol vectors and suffix copies per compressed block is measured
at most 572 712 bytes over the same input classes, and moves less than one block length
across a sixteenfold input range.

A stream that declares more table memory than policy allows is refused with `LimitExceeded`
before the decoder builds any table from it: no table is built and none is held past the
refusal. Construction allocates twice: the two content buffers.

## Parallelism

The codec runs on one thread and keeps no worker state, so the per-thread additional memory
is zero. A threaded caller shares nothing the figures above count twice.

## Reading the API

`Encoder::steady_state_bytes` and `Encoder::peak_bytes` report the streaming figures for
the mode the encoder opened under. `Parser::declared_bytes` and
`Parser::balanced_declared_bytes` report the parse figures per block size. `Mode::Fast` and
`Mode::Balanced` select the contract; `Encoder::new` and `Encoder::with_layout` keep
selecting FAST, and BALANCED selects only through `Encoder::balanced` and
`Encoder::balanced_with_layout`. Nothing mode selecting reaches the stream: the decoder
never learns it.

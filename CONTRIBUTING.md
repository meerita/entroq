# Contributing to Entroq

This page states how to build a change, how to validate it, and how to submit it.

`CODE_OF_CONDUCT.md` applies to every space of this project.

## Before you start

Entroq is in early development and its format is not frozen.

Open an issue before you write a large change. A change that alters the bitstream, a
declared memory bound, or a public API needs agreement first.

## Build

`rust-toolchain.toml` pins the toolchain. Do not change the pin inside a feature change.

Every action enters through the `Makefile`. Do not add a second entry point.

```sh
make help       # list every target
make build      # compile the workspace
make check      # type check the workspace
make fmt        # format the workspace
make lint       # run clippy over the workspace and its targets
```

## Validation

Entroq validates in tiers. Each tier declares an input budget and a time budget.

No validation segment runs longer than 120 seconds. A campaign that needs more time is
split into segments. It is not given more time.

| Tier | When you run it | Input budget | Duration | Recorded |
|---|---|---|---|---|
| `smoke` | every edit | 1 MiB total | 30 s | no |
| `dev` | before you report work complete | 10 MiB per codec path | 120 s | yes |
| `gate` | at a phase exit or a milestone | 100 MiB per codec path | segmented | yes |

```sh
make smoke        # smoke tier
make test         # dev tier, recorded
make gate         # gate tier, segmented and resumable
make gate-resume  # continue the current gate campaign where it stopped
make validate     # format check, lint, and the dev tier
```

Fuzzing runs in bounded segments too. The Fuzzing section below states how.

The format skeleton has a gate campaign of its own. It holds the proofs a cheap tier cannot
carry: round trips across four size classes, every fixture stream cut at every byte offset,
both machines driven at every chunk size, the memory growth curve from one mebibyte to one
gibibyte, the cross-architecture vectors, and one bounded invocation of every fuzz target.

```sh
make skeleton          # every heavy proof, one per segment
make skeleton-resume   # continue the current skeleton campaign where it stopped
```

Both targets compile what the campaign then measures before the first segment starts, so no
segment spends its budget on a compiler. The compile step builds a container image and the
tool inside it, so it needs a container tool and takes a few minutes the first time. Only the
cross-architecture segment needs the container; every other segment runs on the host alone.

A lower tier never substitutes for a higher one. A dev-tier result is not a gate result.

A recorded run writes its record outside the repository, under `../runs` by default. Set
`RUNS` to change that path. The repository carries code, not evidence.

## The competitor laboratory

Entroq is measured against pinned builds of LZ4, Zstandard, Brotli, Snappy, and zlib. They
live outside the repository, under `../lab` by default. Set `LAB` to change that path.

```sh
make lab          # build every pinned competitor, one segment per codec
make lab-resume   # continue the current lab campaign where it stopped
```

`make lab` needs a network, `git`, `cmake`, and a C and C++ compiler. No other target
needs any of them, so you can run every gate without them.

The benchmark harness links the laboratory at build time, and it links every pinned
competitor or none. A host that has not built the laboratory compiles the harness, builds
the laboratory with it, and links it on the next build. Until then the harness measures
nothing and says so; it never reports a partial set of competitors as the set.

```sh
make lab          # the harness builds the laboratory
make bench        # the next build links it, and the campaign measures
```

Every other target works on a host with no laboratory, including the format check, the
lint, the build, the test suite, and the integration lane.

Each competitor is pinned to the commit behind its release tag and built as a static
library with the release configuration its own project recommends. A competitor built with
weaker optimization than Entroq is not a baseline.

Each build carries a `MANIFEST` that says how it was produced, and a `REBUILD` that records
what a rebuild from those commands produced. A build with no manifest cannot appear in a
result.

A pinned build is never rebuilt in place. A new build of the same project takes a new
version directory, so an old result keeps pointing at the build that produced it.

## The corpus

A measurement reads a corpus. The registry that describes every corpus entry is code; the
bytes are not. The cache lives outside the repository, under `../corpus` by default. Set
`CORPUS` to change that path.

```sh
make corpus          # materialize every entry, one segment per group
make corpus-resume   # continue the current corpus campaign where it stopped
```

The registry holds two kinds of entry.

A project entry is generated from a seed the registry records, through a sequence the
repository owns. It reads no clock, no environment, and no container whose iteration order
is unspecified, so one seed produces one set of bytes on every host. `make corpus`
generates every project entry twice, into two directories neither generation shares, and
fails unless both match each other and the digest the registry pins.

A public entry is fetched from its distributor. Its archive is checked against the
recorded checksum as it arrives, and the bytes it unpacks to are checked against the
digest the registry pins. A fetch needs a network, `curl`, and `unzip` or `gzip`.

Every entry states its license and what established it. An entry whose license cannot be
established is not registered, because a published number measured on it would carry a
redistribution claim nobody checked. Two widely used corpora are absent for that reason.

Every entry falls in exactly one size class, and a benchmark tier selects a class:

| Class | Size |
|---|---|
| `tiny` | under 1 KiB |
| `small` | 1 KiB to 64 KiB |
| `medium` | 64 KiB to 4 MiB |
| `large` | 4 MiB to 100 MiB |
| `huge` | 100 MiB and above, streamed |

Prefer many distinct small inputs to one large input. Variety finds more defects per
second than volume.

To see the registry, including the source, checksum, and license of every entry:

```sh
cargo run --release --package entroq-bench -- corpus list --all
```

## The benchmark

The benchmark harness measures every competitor in-process, through the library the
laboratory built, and writes one machine-readable result per segment. It never runs a
competitor's command line tool: a process invocation costs milliseconds, the codec work at
every size class up to the large one costs less, and two of the five competitors publish no
tool at all.

```sh
make bench TIER=<tier>    # a benchmark campaign at one tier
make bench-resume         # continue the current benchmark campaign where it stopped
```

A campaign at a segmented tier runs one segment per competitor, operating point group, and
size class. The cost of one operating point spans three orders of magnitude inside one
project, so the points of a competitor are grouped by cost rather than measured together.
Zstandard at level 22 and Brotli at q11 each cost more over the large class than every
cheaper point of the same competitor put together, and each holds a segment of its own.

Each tier means something different, and a lower tier never stands in for a higher one:

| Tier | What it buys |
|---|---|
| `smoke` | the harness runs and produces a parseable result; the numbers are ignored |
| `dev` | one machine, few samples, direction only, never published |
| `gate` | the full tier corpus, repeated samples, variance reported, gates a milestone |
| `publication` | a controlled machine, pinned versions, full variance, competitor parity checked |

A number leaves the repository only from a sealed publication-tier record. A dev-tier number
never appears in a document, a release note, or a comparison claim.

Every metric is reported as measured, with the call that produced it, or as unavailable, with
the reason. No metric is reported as a zero. The harness links no Entroq codec, so every
result carries an empty Entroq column and says why.

Two reports read what a campaign recorded. Both take a run record directory and read every
result under it.

```sh
make report RESULTS=<record dir>                        # mark the dominated points
make compare BASELINE=<first record> RESULTS=<second>   # did the second reproduce the first
```

`make report` plots each operating point on every axis that has data and marks every point
another point dominates. An axis no result carries a number for reports no data with the
reason, and no other metric stands in its place.

`make compare` states, row by row and metric by metric, whether a second campaign reproduced
a first one. It reads its tolerance from the first record and chooses none of its own. A
number that is a property of the bytes has no variance, so any difference in it is a finding.
A number that is a timing is judged against the spread the first record states for the block
it came from. A number that moves between runs and that no record states a variance for is
reported with its difference and no verdict. Two rows are compared only when they measured
the same bytes, at the same operating point, through the same competitor version, under the
same integrity setting.

Both reports write the document to standard output and the lines a person reads to standard
error, so redirecting standard output gives a file a parser reads.

For the tiers, the size classes, the operating point groups, and what each tier licenses:

```sh
cargo run --release --package entroq-bench -- help
```

## Fuzzing

Every parser and every decoder entry point has a fuzz target. A target is declared before
its driver exists, so the set is fixed and no driver appears without a place in it. A driver
arrives with the code it covers.

```sh
make fuzz-list                 # every target, and whether a driver exists for it
make fuzz TARGET=<name>        # advance one target by one bounded segment
make fuzz-seed                 # seed the stream-reading targets from the format vectors
```

A campaign that names fuzzing as one of its segments advances every runnable target inside
that one segment, with each invocation shortened to fit the budget they share. It writes no
record of its own: the campaign that ran it is the record, and it extends the same corpus.

`make fuzz` needs cargo-fuzz. Install it with `cargo install cargo-fuzz`. No other target
needs it.

Every driver is built with the sanitizer set to none. The pinned toolchain is stable and
AddressSanitizer needs nightly. The coverage instrumentation libFuzzer needs is stable, so a
bounded segment still runs. A memory error that only a sanitizer detects is not detected
here.

Fuzzing is cumulative, not long. One invocation is one bounded segment and then it stops.
Coverage comes from many segments across many days, so each run record states the time the
target has accumulated across every segment so far. That figure is the one to read, not the
duration of one segment.

Each target keeps its corpus at `../runs/fuzz-corpus/<target>/`, outside the repository. The
next segment continues from it instead of starting cold. Set `RUNS` to change that path.

`make fuzz-seed` writes the format vector catalog and copies it into the corpus of the targets
whose input is a whole stream, with the prefix each of those drivers reads in front of it. It
adds and never removes, and it leaves a file the corpus already holds alone. A target that
reads a table description, a block payload, or content is not seeded from a frame, because a
frame is none of those.

Never delete a corpus to start clean. It is coverage that many segments already paid for,
and deleting it discards every hour spent on the target.

A crash writes its reproducer to `../runs/fuzz-corpus/<target>/artifacts/`, beside the
corpus. Minimize it, commit it as a regression fixture, and fix the defect it found. The
fixture lands before the fix does.

## Integration

The same gates run on a clean Linux host, in a container this repository builds. The
container carries the toolchain that `rust-toolchain.toml` names, and nothing of your
machine. It mounts the repository read only, so an integration lane cannot write inside
the repository.

```sh
make ci           # format check, lint, build, and the smoke tier, in the container
make ci-validate  # format check, lint, and the dev tier, in the container, recorded
```

`make ci-validate` writes its record under `RUNS`, the same as `make test`. The record
names the container as the host that produced it.

A lane runs on the platform of the host. Set `PLATFORM` to name another one.

```sh
make ci PLATFORM=linux/amd64
```

A platform that the host must emulate still runs, and the lane states that it is
emulated. An emulated platform shows that the gates run. It is not a result for that
architecture.

Docker is the only host requirement of an integration lane. Every tier also runs directly
on a machine without it.

## Commits and branches

`master` is the integration branch. Keep it buildable and validated.

Develop a non-trivial change on a topic branch. Name the branch after the area it
touches, such as `format/...`, `encode/...`, `decode/...`, `entropy/...`, `matcher/...`,
`parser/...`, `stream/...`, or `bench/...`.

Keep each commit focused on one coherent change. Do not mix unrelated work in one commit.

Write the subject in `subsystem/component: Imperative description` form. Explain in the
body why the change exists, not only what the diff contains.

Every commit that reaches `master` satisfies the validation its scope requires. Do not
leave an intermediate commit broken. Preserve bisectability.

Rebase a private branch freely. Do not rewrite history that another branch already uses
as a base, and do not force-push a shared branch. Correct a published mistake with a new
commit or an explicit revert.

Do not attribute work to a tool. Do not add a generated-by marker or a tool trailer to a
commit, a pull request, or a comment.

## Pull requests

Open the pull request as a draft.

Use the commit subject form for the title.

State what changed, why it changed, which validation ran, and which gaps stay open.

Name the tier each gate ran at, and state whether its run record sealed.

Keep the description short. Do not restate the diff.

## Hard gates

A change that breaks one of these is rejected, not negotiated.

* `decode(encode(x)) == x` for every input, every mode, and every chunking.
* Every mode stays within the memory bound it declares.
* No path requires the whole input or the whole output in memory.
* A decoder can inspect the requirements a stream declares and reject the stream before
  it allocates for it.
* No input causes a panic across the public API, memory unsafety, unbounded allocation,
  or an infinite loop.
* The same bytes decode identically on every supported architecture.
* A frozen format version keeps decoding what it accepted.
* Every published number is reproducible from its recorded inputs.

A comparison against another codec names that codec's version, its settings, the corpus,
and the machine, or it is not a comparison.

## License

You license your contribution under both the Apache-2.0 license and the MIT license, as
`README.md` states. You add no other term or condition.

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

A lower tier never substitutes for a higher one. A dev-tier result is not a gate result.

A recorded run writes its record outside the repository, under `../runs` by default. Set
`RUNS` to change that path. The repository carries code, not evidence.

Prefer many distinct small inputs to one large input. Variety finds more defects per
second than volume.

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

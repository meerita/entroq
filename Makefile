# Entroq repository entry point.
#
# Each target delegates to the tool that owns the implementation. Do not
# duplicate delegated logic here.
#
# The fuzz drivers do not exist yet, so `fuzz` says so and fails. Every other
# target works.
#
# `lab` builds the competitor laboratory. It needs a network, a C and C++
# compiler, cmake, and git. No other target needs any of them, so a host
# without them still runs every gate.
#
# `corpus` materializes the corpus. It generates the project corpus from
# recorded seeds, which needs nothing, and fetches the registered public
# corpora, which needs a network, curl, unzip, and gzip. No corpus byte is
# committed: the registry and the method of obtaining it are.
#
# No validation segment may exceed 120 seconds.
#
# Build inputs:
#
#   CARGO   The cargo binary that every target invokes.
#           Scope:    every target in this file.
#           Required: no.
#           Default:  cargo, resolved from PATH, which `rust-toolchain.toml`
#                     pins to the declared channel.
#
#   LAB     The competitor codec workspace. Holds the pinned, built
#           compression systems that Entroq is compared against. `lab`
#           produces it. The benchmark harness links the libraries it names,
#           so it is a build input of the workspace and not only of a
#           comparison. It is exported for every target, because two targets
#           that disagree about it would rebuild the harness in turn.
#           A workspace whose LAB holds no complete set of competitors builds
#           a harness that links none and says so when asked to measure.
#           Scope:    every target that builds or measures.
#           Required: yes, for a comparison.
#           Default:  ../lab
#
#   CORPUS  The corpus cache. Holds the generated project corpus and the
#           fetched public corpora that a measurement reads. `corpus`
#           produces it. It is outside the repository on purpose: the
#           registry is code, and the bytes it describes are not.
#           Scope:    corpus, corpus-resume, bench.
#           Required: no.
#           Default:  ../corpus
#
#   RUNS    The run record root. Recorded tiers write their manifest,
#           journal, and raw segment output here. It is outside the
#           repository on purpose: the repository carries code, not evidence.
#           Scope:    test, gate, gate-resume, fuzz, bench.
#           Required: no.
#           Default:  ../runs
#
#   TIER    The validation tier a recorded target runs at.
#           Scope:    bench, bench-resume.
#           Required: no.
#           Default:  dev
#
#   TARGET  The fuzz target to advance by one segment.
#           Scope:    fuzz.
#           Required: yes, for fuzz.
#
#   PLATFORM
#           The platform an integration lane runs on: linux/amd64 or
#           linux/arm64.
#           Scope:    ci, ci-validate.
#           Required: no.
#           Default:  the platform of the host. A platform the host must
#                     emulate still runs, and the lane reports it as
#                     emulated. An emulated platform is not an architecture
#                     result.

CARGO ?= cargo
LAB ?= ../lab
export LAB
CORPUS ?= ../corpus
RUNS ?= ../runs
TIER ?= dev
PLATFORM ?=

# Repository tooling, versioned with the code whose gates it runs.
RUNNER = $(CARGO) run --quiet --release --package entroq-run --

.PHONY: help build check fmt fmt-check lint smoke test gate gate-resume lab lab-resume corpus corpus-resume fuzz bench-harness bench bench-resume validate ci ci-validate clean

help:
	@echo "build      compile the workspace"
	@echo "check      type-check the workspace without producing artifacts"
	@echo "fmt        format the workspace"
	@echo "fmt-check  fail when the workspace is not formatted"
	@echo "lint       run clippy over the workspace and its targets"
	@echo "smoke      smoke tier: 30 s, 1 MiB inputs, not recorded"
	@echo "test       dev tier: 120 s, 10 MiB inputs, recorded"
	@echo "gate       gate tier: 100 MiB inputs, segmented and resumable"
	@echo "gate-resume  continue the current gate campaign where it stopped"
	@echo "lab        build the pinned competitors into LAB, one segment per codec"
	@echo "lab-resume   continue the current lab campaign where it stopped"
	@echo "corpus     materialize the corpus into CORPUS, one segment per group"
	@echo "corpus-resume  continue the current corpus campaign where it stopped"
	@echo "fuzz       advance one fuzz target by one bounded segment (TARGET=<name>)"
	@echo "bench      benchmark campaign at TIER, one segment per competitor"
	@echo "bench-resume continue the current benchmark campaign where it stopped"
	@echo "validate   fmt-check, lint, and the dev tier"
	@echo "ci         fmt-check, lint, build, and the smoke tier, in a clean container"
	@echo "ci-validate  validate in a clean container, recorded"
	@echo "clean      remove build artifacts"

build:
	$(CARGO) build --workspace

check:
	$(CARGO) check --workspace

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

lint:
	$(CARGO) clippy --workspace --all-targets

# Tiered validation. Each target honors the budget its tier declares.
# Every tier except smoke writes an append-only record under $(RUNS).

smoke:
	$(RUNNER) run --tier smoke

test:
	$(RUNNER) run --tier dev --runs $(RUNS)

gate:
	$(RUNNER) run --tier gate --runs $(RUNS)

gate-resume:
	$(RUNNER) resume --tier gate --runs $(RUNS)

# The competitor laboratory. One segment per codec, so one build fits the
# segment budget and a resumed campaign rebuilds only what did not pass.
#
# Each segment builds its competitor at its pinned commit and then rebuilds it
# from its own recorded commands into a clean prefix. A rebuild that does not
# reproduce the recorded bytes is recorded, not failed: a compiler and an
# archiver embed a path and a timestamp, so two builds of one source tree can
# behave the same and not hash the same.

lab:
	$(RUNNER) run --suite lab --tier gate --runs $(RUNS)

lab-resume:
	$(RUNNER) resume --suite lab --tier gate --runs $(RUNS)

# The corpus. One segment per group, so one fetch fits the segment budget and
# a resumed campaign re-fetches only what did not pass.
#
# A generated entry is written from the seed the registry records, and is then
# generated twice into two directories to prove the seed is a pin. A fetched
# entry is checked against its recorded checksum as the archive arrives and
# again on the bytes it unpacks to, so a broken transfer and a drifted upstream
# do not look alike.

corpus:
	CORPUS=$(CORPUS) $(RUNNER) run --suite corpus --tier gate --runs $(RUNS)

corpus-resume:
	CORPUS=$(CORPUS) $(RUNNER) resume --suite corpus --tier gate --runs $(RUNS)

fuzz:
	@test -n "$(TARGET)" || { echo "make fuzz: set TARGET=<fuzz target>" >&2; exit 1; }
	$(RUNNER) fuzz --target $(TARGET) --runs $(RUNS)

# The benchmark. One segment per competitor, and one per competitor and size
# class at a segmented tier, so a segment holds one budget and a resumed campaign
# re-measures only what did not pass.
#
# Each segment measures its competitor in-process, through the library built into
# $(LAB), and writes one machine-readable result into the segment's evidence
# directory. Entroq has no codec path yet, so every result carries an empty
# Entroq column and states why.
#
# The smoke tier is not recorded, so it takes no run root.
#
# The harness is built before the campaign starts. A campaign measures codec
# work under a wall-clock budget, and a compile inside the first segment would
# spend that budget on the compiler.

RECORDED_RUNS = $(if $(filter smoke,$(TIER)),,--runs $(RUNS))

bench-harness:
	$(CARGO) build --quiet --release --package entroq-bench

bench: bench-harness
	CORPUS=$(CORPUS) $(RUNNER) run --suite bench --tier $(TIER) $(RECORDED_RUNS)

bench-resume: bench-harness
	CORPUS=$(CORPUS) $(RUNNER) resume --suite bench --tier $(TIER) --runs $(RUNS)

validate: fmt-check lint test

# Integration. The same gates run on a clean Linux host that carries the
# pinned toolchain and nothing of the developer's machine. The container
# mounts the repository read only, so a lane cannot write inside it.

ci:
	PLATFORM=$(PLATFORM) ci/ci.sh gates

ci-validate:
	PLATFORM=$(PLATFORM) RUNS=$(RUNS) ci/ci.sh validate

clean:
	$(CARGO) clean

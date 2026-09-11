# Entroq repository entry point.
#
# Each target delegates to the tool that owns the implementation. Do not
# duplicate delegated logic here.
#
# The fuzz drivers and the benchmark harness do not exist yet, so `fuzz` and
# `bench` say so and fail. Every other target works.
#
# `lab` builds the competitor laboratory. It needs a network, a C and C++
# compiler, cmake, and git. No other target needs any of them, so a host
# without them still runs every gate.
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
#           produces it. Every comparison target reads it.
#           Scope:    lab, lab-resume, bench, and any comparison target.
#           Required: yes, for a comparison.
#           Default:  ../lab
#
#   RUNS    The run record root. Recorded tiers write their manifest,
#           journal, and raw segment output here. It is outside the
#           repository on purpose: the repository carries code, not evidence.
#           Scope:    test, gate, gate-resume, fuzz, bench.
#           Required: no.
#           Default:  ../runs
#
#   TIER    The validation tier a recorded target runs at.
#           Scope:    bench.
#           Required: no.
#           Default:  dev
#
#   TARGET  The fuzz target to advance by one segment.
#           Scope:    fuzz.
#           Required: yes, for fuzz.

CARGO ?= cargo
LAB ?= ../lab
RUNS ?= ../runs
TIER ?= dev

# Repository tooling, versioned with the code whose gates it runs.
RUNNER = $(CARGO) run --quiet --release --package entroq-run --

.PHONY: help build check fmt fmt-check lint smoke test gate gate-resume fuzz bench validate clean

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
	@echo "fuzz       advance one fuzz target by one bounded segment (TARGET=<name>)"
	@echo "bench      benchmark campaign at TIER"
	@echo "validate   fmt-check, lint, and the dev tier"
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
	LAB=$(LAB) $(RUNNER) run --suite lab --tier gate --runs $(RUNS)

lab-resume:
	LAB=$(LAB) $(RUNNER) resume --suite lab --tier gate --runs $(RUNS)

fuzz:
	@test -n "$(TARGET)" || { echo "make fuzz: set TARGET=<fuzz target>" >&2; exit 1; }
	$(RUNNER) fuzz --target $(TARGET) --runs $(RUNS)

bench:
	$(RUNNER) bench --tier $(TIER) --lab $(LAB) --runs $(RUNS)

validate: fmt-check lint test

clean:
	$(CARGO) clean

#!/bin/sh
# Runs one lane of the Entroq gates in a clean container.
#
# The container is the integration host. It holds the toolchain that
# rust-toolchain.toml names, it mounts the repository read only, and it keeps
# its build output in a volume. A lane cannot write inside the repository.
#
# Usage:
#   ci/ci.sh gates       format check, lint, build, and the smoke tier
#   ci/ci.sh validate    format check, lint, and the dev tier, recorded
#
# Inputs:
#
#   DOCKER    The container tool the lane invokes.
#             Required: no.
#             Default:  docker.
#
#   PLATFORM  The platform the lane runs on: linux/amd64 or linux/arm64.
#             Required: no.
#             Default:  the platform of the host.
#             A platform the host must emulate still runs, and the lane
#             reports it as emulated. An emulated platform shows that the
#             gates run. It is not an architecture result.
#
#   IMAGE     The image the lane builds and runs. The platform becomes its tag.
#             Required: no.
#             Default:  entroq-ci.
#
#   RUNS      The run record root the validate lane writes to. It is outside
#             the repository on purpose: the repository carries code, not
#             evidence.
#             Required: no.
#             Default:  ../runs, beside the repository.
#
# Exit status:
#   0  the lane passed
#   1  a gate failed
#   2  the lane could not run

set -eu

lane=${1:-}
case $lane in
    gates | validate) ;;
    *)
        echo "ci: name a lane: gates or validate" >&2
        exit 2
        ;;
esac

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
docker=${DOCKER:-docker}
image=${IMAGE:-entroq-ci}

host=$("$docker" info --format '{{.Architecture}}')
case $host in
    aarch64 | arm64) host_platform=linux/arm64 ;;
    x86_64 | amd64) host_platform=linux/amd64 ;;
    *)
        echo "ci: the host reports the architecture $host, which this lane does not know" >&2
        exit 2
        ;;
esac

platform=${PLATFORM:-$host_platform}
case $platform in
    linux/amd64 | linux/arm64) ;;
    *)
        echo "ci: PLATFORM is linux/amd64 or linux/arm64" >&2
        exit 2
        ;;
esac
arch=${platform#linux/}

if [ "$platform" = "$host_platform" ]; then
    echo "ci: lane $lane on $platform, native."
else
    echo "ci: lane $lane on $platform, emulated by a $host_platform host."
    echo "ci: an emulated platform shows the gates run. It is not an architecture result."
fi

"$docker" build \
    --platform "$platform" \
    --file "$root/ci/Dockerfile" \
    --tag "$image:$arch" \
    "$root"

set -- run --rm \
    --platform "$platform" \
    --volume "$root:/work:ro" \
    --volume "entroq-ci-target-$arch:/ci/target" \
    --volume "entroq-ci-registry-$arch:/opt/cargo/registry" \
    --env CARGO_TARGET_DIR=/ci/target

# Every lane states the toolchain it resolved, and the file it resolved it from.
if [ "$lane" = validate ]; then
    runs=${RUNS:-$root/../runs}
    mkdir -p "$runs"
    runs=$(CDPATH='' cd -- "$runs" && pwd)
    set -- "$@" --volume "$runs:/runs" "$image:$arch" \
        sh -c 'rustup show active-toolchain && make validate RUNS=/runs'
else
    set -- "$@" "$image:$arch" \
        sh -c 'rustup show active-toolchain && make fmt-check lint build smoke'
fi

exec "$docker" "$@"

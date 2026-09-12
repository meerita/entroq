#!/bin/sh
# Settles whether two architectures write the same Entroq bytes and read each
# other's.
#
# One lane writes the format vector catalog natively. The other writes it in a
# container on a different platform. Each lane then compares both sets and
# decodes the other's output. A difference in either direction is a failure.
#
# Byte order is architectural, not microarchitectural, so a platform the host
# emulates settles this question. No timing is read here and no performance
# claim is made from either lane.
#
# Usage:
#   ci/byteorder.sh prepare    build the image and both binaries
#   ci/byteorder.sh run        write both vector sets and compare them
#
# `prepare` compiles. `run` does not: it invokes the binaries `prepare` built
# and never cargo. A compile inside a measured segment would spend the budget
# on the compiler, and on a host where another build holds the package cache
# lock even a no-op cargo invocation waits minutes for it. Run `prepare` first,
# outside any budget.
#
# Inputs:
#
#   DOCKER    The container tool the lane invokes.
#             Required: no.
#             Default:  docker.
#
#   PLATFORM  The platform the other lane runs on: linux/amd64 or linux/arm64.
#             It must differ from the host's architecture, or the run compares
#             one architecture with itself and proves nothing.
#             Required: no.
#             Default:  linux/amd64.
#
#   IMAGE     The image the lane builds and runs. The platform becomes its tag.
#             Required: no.
#             Default:  entroq-ci.
#
#   VECTORS   The directory both lanes write their vectors into. It is outside
#             the repository on purpose: the repository carries code, not
#             evidence.
#             Required: no.
#             Default:  ../runs/byteorder, beside the repository.
#
# Exit status:
#   0  the two lanes agree
#   1  the two lanes do not agree
#   2  the lane could not run

set -eu

lane=${1:-}
case $lane in
    prepare | run) ;;
    *)
        echo "byteorder: name a lane: prepare or run" >&2
        exit 2
        ;;
esac

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
docker=${DOCKER:-docker}
image=${IMAGE:-entroq-ci}
cargo=${CARGO:-cargo}

# The binary each lane runs. The host's sits in the target directory cargo
# writes to; the other lane's sits in the volume its container mounts.
native_tool=${CARGO_TARGET_DIR:-$root/target}/release/entroq-proof
other_tool=/ci/target/release/entroq-proof

host=$("$docker" info --format '{{.Architecture}}')
case $host in
    aarch64 | arm64) host_platform=linux/arm64 ;;
    x86_64 | amd64) host_platform=linux/amd64 ;;
    *)
        echo "byteorder: the host reports the architecture $host, which this lane does not know" >&2
        exit 2
        ;;
esac

platform=${PLATFORM:-linux/amd64}
case $platform in
    linux/amd64 | linux/arm64) ;;
    *)
        echo "byteorder: PLATFORM is linux/amd64 or linux/arm64" >&2
        exit 2
        ;;
esac
arch=${platform#linux/}

if [ "$platform" = "$host_platform" ]; then
    echo "byteorder: $platform is the host's own architecture, so the two lanes would be one" >&2
    exit 2
fi

vectors=${VECTORS:-$root/../runs/byteorder}
mkdir -p "$vectors"
vectors=$(CDPATH='' cd -- "$vectors" && pwd)
native="$vectors/native"
other="$vectors/$arch"

container() {
    "$docker" run --rm \
        --platform "$platform" \
        --volume "$root:/work:ro" \
        --volume "entroq-ci-target-$arch:/ci/target" \
        --volume "entroq-ci-registry-$arch:/opt/cargo/registry" \
        --volume "$vectors:/vectors" \
        --env CARGO_TARGET_DIR=/ci/target \
        "$image:$arch" \
        sh -c "$1"
}

if [ "$lane" = prepare ]; then
    echo "byteorder: building the $platform image, emulated by a $host_platform host."
    "$docker" build \
        --platform "$platform" \
        --file "$root/ci/Dockerfile" \
        --tag "$image:$arch" \
        "$root"
    echo "byteorder: building the tool on both lanes."
    (cd "$root" && "$cargo" build --release --quiet --package entroq-proof)
    test -x "$native_tool" || {
        echo "byteorder: the build left no $native_tool" >&2
        exit 2
    }
    container "cargo build --release --quiet --package entroq-proof && test -x $other_tool"
    exit 0
fi

test -x "$native_tool" || {
    echo "byteorder: $native_tool is not built. Run ci/byteorder.sh prepare first." >&2
    exit 2
}

rm -rf "$native" "$other"

echo "byteorder: $host_platform writes $native"
"$native_tool" vectors --out "$native"

# One container start, because starting one under emulation costs more than the
# work inside it. The other lane writes its vectors and then reads the host's.
echo "byteorder: $platform writes $other and reads the other lane's."
container "$other_tool vectors --out /vectors/$arch \
    && $other_tool cross --mine /vectors/$arch --theirs /vectors/native"

echo "byteorder: $host_platform compares both sets and decodes the other's."
"$native_tool" cross --mine "$native" --theirs "$other"

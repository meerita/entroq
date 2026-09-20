---
title: Benchmarks
description: What Entroq measures, against what, how, and what a number from each tier licenses.
class: reference
audience: someone reading, reproducing, or citing an Entroq measurement
order: 2
version_axes: [encoder_version, harness_version]
encoder_version: 0.1.0. Two encoder modes, FAST and BALANCED, at a window of 65 536 bytes and a block of 65 536 input bytes.
harness_version: 0.0.0
---

# Benchmarks

This page states the measurement laboratory as it exists now, and the measurement of Entroq
beside the competitors it is measured against.

Every number below comes from a sealed gate-tier record. **The gate tier is not the
publication tier, and nothing on this page is a claim that one codec beats another.** A
comparison claim needs a controlled machine and a publication-tier record, and neither exists
yet. Read every number with the scope stated beside it, and read the timing stability section
before reading any throughput as a property of a codec rather than of this machine.

## The measurement pipeline

```mermaid
flowchart LR
    U[pinned upstream source] --> L[laboratory build]
    L -->|static library + MANIFEST| H[benchmark harness]
    E[the codec crate] -->|built with the harness| H
    R[corpus registry] -->|seeds and checksums| C[corpus cache]
    C -->|checked bytes| H
    H -->|one result document per segment| D[(run record)]
    D --> P[parser]
    D --> F[frontier report]
    D --> M[comparison]
```

The laboratory and the corpus cache live outside the repository. The registry that describes
the corpus and the pin that names each competitor commit are in the repository; the bytes
they describe are not.

The harness measures every codec in-process. A competitor goes through the library that
project publishes, linked from the laboratory at build time; Entroq goes through the crate of
this workspace, built with the harness. Both cross one call and one process, so neither side
of a comparison is charged for a boundary the other does not pay.

It does not run a competitor's command line tool: a process invocation costs milliseconds, the
codec work at every size class up to the large one costs less, and two of the five competitors
publish no tool at all. There is no Entroq command line tool to run either.

## Competitors

Each competitor is built from the commit behind its release tag, as a static library, with
the release configuration its own project recommends.

| Codec | Version | Upstream commit |
|---|---|---|
| LZ4 | v1.10.0 | `ebb370ca83af193212df4dcbadcc5d87bc0de2f0` |
| Zstandard | v1.5.7 | `f8745da6ff1ad1e7bab384bd1f9d742439278e99` |
| Brotli | v1.2.0 | `028fb5a23661f123017c060daa546b55cf4bde29` |
| Snappy | 1.2.2 | `6af9287fbdb913f0794d0148c6aa43b58e63c8e3` |
| zlib | v1.3.2 | `da607da739fa6047df13e66a2af6b8bec7c2a498` |

A build carries a manifest stating how it was produced, including the build command, the
compiler version, the architecture, and the checksum of every library the linker is given. A
build with no manifest cannot appear in a result. A pinned build is never rebuilt in place: a
new build of the same project takes a new version directory, so an old result keeps pointing
at the build that produced it.

A rebuild from a manifest reproduces behavior, not bytes. Every rebuilt archive has exactly
the size of the one it was compared against and differs from it in a few tens of bytes out of
tens or hundreds of kilobytes. Each build records that measurement: the size, the differing
byte count, and the first differing offsets. The manifest checksum identifies the artifact a
result was produced with. It is not a claim that the build is bit reproducible.

## Operating points

A competitor is measured across the points its project exposes, not at one default level.
Entroq exposes two, in one group: FAST is the default, a single-entry match table of 16 384
entries behind a tag gate, an adaptive skip over searched misses, and a greedy parse. BALANCED
is a bounded hash chain at depth 32 feeding a length-lazy depth-1 parse, carrying the same
skip.

| Codec | Points | Groups |
|---|---|---|
| Entroq | `fast`, `balanced` | `default` |
| LZ4 | `fast-1`, `fast-3`, `fast-5`, `fast-9`, `hc-1`, `hc-4`, `hc-9`, `hc-12` | `fast`, `hc` |
| Zstandard | `level-1`, `level-3`, `level-6`, `level-9`, `level-12`, `level-15`, `level-19`, `level-22` | `low`, `high`, `max` |
| Brotli | `q0`, `q2`, `q5`, `q9`, `q11` | `low`, `high`, `max` |
| Snappy | `default` | `default` |
| zlib | `level-1`, `level-6`, `level-9` | `low`, `high` |

The cost of one point spans three orders of magnitude inside one project, so the points of a
competitor are grouped by cost. A segmented campaign measures one group and one size class per
segment, which is what keeps each segment inside its budget.

## Corpus

The registry states the source, the checksum, the size, the class, and the license of every
entry. A project entry is generated from a recorded seed through a sequence this repository
owns, and generating it twice into two directories produces the same bytes. A public entry is
fetched from its distributor and checked against a recorded checksum as the archive arrives
and again on the bytes it unpacks to.

An entry whose license cannot be established is not registered.

| Group | Entries | Classes covered | License |
|---|---|---|---|
| `project` | 19 generated entries: JSON, source, logs, database rows, serialized binary, mixed, short tokens, long repetitions, sparse, zeros, high entropy, already compressed, and a large blob | tiny, small, medium, large | MIT OR Apache-2.0 |
| `gutenberg` | 3 English literary texts | medium, large | Project Gutenberg License |
| `enwik` | the enwik8 snapshot | large | GFDL-1.2-only |
| `sourcecode` | `go-source`, a pinned source tree | huge | BSD-3-Clause |

| Class | Size |
|---|---|
| `tiny` | under 1 KiB |
| `small` | 1 KiB to 64 KiB |
| `medium` | 64 KiB to 4 MiB |
| `large` | 4 MiB to 100 MiB |
| `huge` | 100 MiB and above, streamed |

Run `entroq-bench corpus list --all` for the registry, including the checksum and license of
every entry.

## Metrics

Every metric is reported with the call that produced it, or as unavailable with the reason.
No metric is reported as a zero, and no absent metric is omitted.

| Metric | Unit | Entroq | Competitors |
|---|---|---|---|
| `compressed_bytes` | bytes | measured | measured |
| `compression_ratio` | input bytes per compressed byte | measured | measured |
| `encode_throughput` | bytes per second | measured | measured |
| `decode_throughput` | bytes per second | measured | measured |
| `peak_rss` | bytes | measured, for the harness process | measured, for the harness process |
| `codec_owned_bytes` | bytes | measured, from `Encoder::steady_state_bytes` | measured for Brotli, LZ4, zlib and Zstandard. Snappy publishes no state-size call and no allocator hook |
| `decoder_owned_bytes` | bytes | measured, from `Decoder::steady_state_bytes` | measured for Zstandard alone. Brotli, LZ4, Snappy and zlib publish no decoder state size |
| `allocations` | allocations | measured through the harness allocator, which every Entroq allocation passes through, on one entry per size class | measured through the harness allocator, for Brotli, LZ4, zlib and Zstandard, on one entry per size class |
| `encode_first_output_latency` | nanoseconds | measured through the streaming encoder, on one entry per size class | measured for Brotli, LZ4, zlib and Zstandard, on one entry per size class. Snappy publishes no streaming interface |
| `encode_streaming_latency` | nanoseconds | measured on the same basis | measured on the same basis |
| `parallel_scaling` | ratio | N/A, no parallel path. This revision is single threaded and publishes no thread parameter | measured for Zstandard alone. Brotli, LZ4, Snappy and zlib publish no thread parameter |
| `encode_cycles_per_byte` | cycles per byte | N/A on every host this project reaches | N/A on every host this project reaches |
| `decode_cycles_per_byte` | cycles per byte | N/A on every host this project reaches | N/A on every host this project reaches |
| `instructions_per_byte` | instructions per byte | N/A on every host this project reaches | N/A on every host this project reaches |
| `random_range_latency` | nanoseconds | N/A, no range read | N/A, no competitor measured here publishes a range read |
| `range_amplification` | ratio | N/A, no range read | N/A, same reason |

The three counter metrics need a performance monitor unit the process may read. The
performance monitor registers trap at the user exception level on the development host,
`perf_event_open` is a Linux call that does not exist on it, and a shared runner usually
denies the counter. A cycle count is never derived from elapsed time and a nominal frequency:
a host that scales frequency, or that mixes core types, makes that product a number with no
meaning.

## Measured numbers

Source: a gate-tier campaign sealed at one revision, 48 of 48 segments, 197 seconds. It holds
594 measured rows over 22 corpus entries: 44 Entroq rows, two per entry at the two operating
points this revision exposes, and 550 competitor rows. The six tables below are the entries
that show the most about codec behavior; the campaign measured the rest.

Every row of every table shares this scope:

```text
machine       Apple M1 Pro, 10 cores, 16 GiB, macOS Darwin 25.5.0, aarch64
build         release profile, rustc 1.98.1
Entroq        encoder version 0.1.0. One frame, resource class small, regions independent,
              a window of 65 536 bytes, a region of 1 048 576 input bytes, a block of
              65 536 input bytes. FAST: a single-entry match table of 16 384 entries behind a
              tag gate, an adaptive skip over searched misses, and a greedy parse. BALANCED:
              a bounded hash chain at depth 32 feeding a length-lazy depth-1 parse, carrying
              the same skip
competitors   LZ4 v1.10.0, Zstandard v1.5.7, Brotli v1.2.0, Snappy 1.2.2, zlib v1.3.2,
              each built from the commit named above
threads       1
integrity     none, for all six
measurement   the call alone, in-process, median of the samples the tier bought
tier          gate. Not a published number, and not a claim about any codec
```

`Ratio` is input bytes per compressed byte, so higher compresses more. `spread` is the
slowest sample minus the fastest, over the median, inside one segment. It is a within-run
figure and it does not bound the movement between runs. A spread above about 0.15 means the
throughput beside it did not repeat closely even within its own segment.

### project-source-small

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `fast` | 10.719 | 3057 | 307.5 | 0.203 | 517.5 | 0.274 |
| Entroq | `balanced` | 18.094 | 1811 | 141.1 | 0.189 | 807.4 | 0.151 |
| LZ4 | `fast-1` | 5.587 | 5865 | 1880.6 | 0.157 | 6879.7 | 0.232 |
| LZ4 | `fast-3` | 5.599 | 5852 | 1960.5 | 0.148 | 7246.4 | 0.196 |
| LZ4 | `fast-5` | 5.496 | 5962 | 1921.8 | 0.155 | 7294.7 | 0.155 |
| LZ4 | `fast-9` | 5.351 | 6124 | 1989.7 | 0.119 | 7433.8 | 0.219 |
| LZ4 | `hc-1` | 8.478 | 3865 | 2179.9 | 0.040 | 11089.0 | 0.103 |
| LZ4 | `hc-4` | 14.397 | 2276 | 512.9 | 0.185 | 19859.4 | 0.093 |
| LZ4 | `hc-9` | 14.888 | 2201 | 373.5 | 0.297 | 20608.8 | 0.098 |
| LZ4 | `hc-12` | 15.170 | 2160 | 48.2 | 0.111 | 21614.8 | 0.111 |
| Snappy | `default` | 6.861 | 4776 | 2604.6 | 0.085 | 6676.4 | 0.158 |
| zlib | `level-1` | 10.304 | 3180 | 970.7 | 0.333 | 1535.6 | 0.035 |
| zlib | `level-6` | 19.692 | 1664 | 284.4 | 0.195 | 2197.4 | 0.113 |
| zlib | `level-9` | 19.859 | 1650 | 182.0 | 0.218 | 2338.2 | 0.102 |
| Zstandard | `level-1` | 11.130 | 2944 | 1055.3 | 0.131 | 2949.9 | 0.027 |
| Zstandard | `level-3` | 14.309 | 2290 | 1519.6 | 0.024 | 3923.4 | 0.303 |
| Zstandard | `level-6` | 16.744 | 1957 | 325.3 | 0.247 | 4537.2 | 0.167 |
| Zstandard | `level-9` | 19.230 | 1704 | 194.1 | 0.302 | 5317.8 | 0.057 |
| Zstandard | `level-12` | 19.692 | 1664 | 71.9 | 0.149 | 6018.0 | 0.095 |
| Zstandard | `level-15` | 20.818 | 1574 | 10.5 | 0.067 | 5465.0 | 0.060 |
| Zstandard | `level-19` | 21.347 | 1535 | 1.9 | 0.061 | 5527.7 | 0.041 |
| Zstandard | `level-22` | 21.347 | 1535 | 1.8 | 0.040 | 5522.1 | 0.106 |
| Brotli | `q0` | 8.219 | 3987 | 1256.7 | 0.052 | 1128.6 | 0.055 |
| Brotli | `q2` | 11.405 | 2873 | 521.0 | 0.229 | 1411.4 | 0.254 |
| Brotli | `q5` | 19.219 | 1705 | 273.4 | 0.170 | 2705.9 | 0.159 |
| Brotli | `q9` | 21.154 | 1549 | 210.9 | 0.311 | 2931.7 | 0.129 |
| Brotli | `q11` | 22.291 | 1470 | 0.8 | 0.009 | 2714.2 | 0.055 |

### project-json-medium

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `fast` | 4.863 | 53905 | 141.8 | 0.096 | 268.5 | 0.181 |
| Entroq | `balanced` | 6.521 | 40202 | 52.1 | 0.143 | 389.6 | 0.216 |
| LZ4 | `fast-1` | 3.641 | 71999 | 878.8 | 0.211 | 4411.9 | 0.092 |
| LZ4 | `fast-3` | 3.455 | 75871 | 986.9 | 0.113 | 4688.2 | 0.055 |
| LZ4 | `fast-5` | 3.396 | 77203 | 975.1 | 0.296 | 4762.6 | 0.049 |
| LZ4 | `fast-9` | 3.292 | 79641 | 968.8 | 0.149 | 4862.0 | 0.073 |
| LZ4 | `hc-1` | 3.886 | 67466 | 567.9 | 0.101 | 4646.5 | 0.056 |
| LZ4 | `hc-4` | 4.880 | 53723 | 164.9 | 0.128 | 5798.6 | 0.109 |
| LZ4 | `hc-9` | 5.330 | 49180 | 49.8 | 0.123 | 7085.0 | 0.080 |
| LZ4 | `hc-12` | 5.412 | 48438 | 17.5 | 0.120 | 6743.3 | 0.154 |
| Snappy | `default` | 3.397 | 77166 | 1100.1 | 0.105 | 3681.3 | 0.096 |
| zlib | `level-1` | 4.801 | 54601 | 311.6 | 0.032 | 750.6 | 0.052 |
| zlib | `level-6` | 6.515 | 40236 | 95.0 | 0.054 | 918.1 | 0.056 |
| zlib | `level-9` | 6.831 | 38376 | 33.5 | 0.045 | 910.1 | 0.034 |
| Zstandard | `level-1` | 5.595 | 46852 | 706.0 | 0.168 | 1867.5 | 0.288 |
| Zstandard | `level-3` | 5.478 | 47853 | 539.8 | 0.302 | 1978.4 | 0.076 |
| Zstandard | `level-6` | 6.423 | 40816 | 135.4 | 0.226 | 2507.5 | 0.059 |
| Zstandard | `level-9` | 7.068 | 37089 | 71.0 | 0.154 | 2888.6 | 0.085 |
| Zstandard | `level-12` | 7.531 | 34809 | 23.6 | 0.259 | 3153.6 | 0.085 |
| Zstandard | `level-15` | 7.932 | 33049 | 6.9 | 0.058 | 3069.0 | 0.127 |
| Zstandard | `level-19` | 7.995 | 32787 | 3.2 | 0.062 | 3024.7 | 0.509 |
| Zstandard | `level-22` | 7.995 | 32787 | 2.4 | 0.051 | 3035.0 | 0.201 |
| Brotli | `q0` | 4.313 | 60785 | 760.1 | 0.161 | 646.5 | 0.204 |
| Brotli | `q2` | 5.575 | 47018 | 274.9 | 0.241 | 764.2 | 0.087 |
| Brotli | `q5` | 6.706 | 39092 | 94.0 | 0.080 | 870.3 | 0.049 |
| Brotli | `q9` | 7.229 | 36263 | 53.5 | 0.200 | 955.7 | 0.070 |
| Brotli | `q11` | 8.416 | 31147 | 0.9 | 0.032 | 826.8 | 0.042 |

### project-source-medium

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `fast` | 12.233 | 85717 | 287.9 | 0.137 | 488.9 | 0.109 |
| Entroq | `balanced` | 27.881 | 37609 | 117.3 | 0.085 | 919.9 | 0.093 |
| LZ4 | `fast-1` | 6.026 | 174021 | 1784.4 | 0.076 | 6010.5 | 0.082 |
| LZ4 | `fast-3` | 6.015 | 174315 | 1873.0 | 0.040 | 5566.4 | 0.272 |
| LZ4 | `fast-5` | 6.005 | 174622 | 1860.6 | 0.063 | 6247.7 | 0.109 |
| LZ4 | `fast-9` | 6.000 | 174773 | 1749.2 | 0.187 | 6235.4 | 0.207 |
| LZ4 | `hc-1` | 9.422 | 111293 | 2144.5 | 0.145 | 8192.0 | 0.024 |
| LZ4 | `hc-4` | 20.108 | 52146 | 405.0 | 0.144 | 13052.7 | 0.120 |
| LZ4 | `hc-9` | 24.835 | 42221 | 138.2 | 0.115 | 16589.0 | 0.088 |
| LZ4 | `hc-12` | 26.209 | 40008 | 38.3 | 0.025 | 15817.5 | 0.071 |
| Snappy | `default` | 7.196 | 145711 | 2426.8 | 0.056 | 6326.3 | 0.162 |
| zlib | `level-1` | 11.528 | 90957 | 683.1 | 0.032 | 1438.3 | 0.022 |
| zlib | `level-6` | 29.997 | 34956 | 245.4 | 0.040 | 2524.2 | 0.033 |
| zlib | `level-9` | 31.104 | 33712 | 127.7 | 0.067 | 2559.6 | 0.131 |
| Zstandard | `level-1` | 14.677 | 71445 | 1521.1 | 0.125 | 3737.7 | 0.053 |
| Zstandard | `level-3` | 17.101 | 61318 | 1604.5 | 0.220 | 4550.0 | 0.075 |
| Zstandard | `level-6` | 23.171 | 45253 | 317.3 | 0.080 | 6304.1 | 0.128 |
| Zstandard | `level-9` | 28.063 | 37365 | 232.3 | 0.139 | 8133.7 | 0.102 |
| Zstandard | `level-12` | 33.152 | 31629 | 137.1 | 0.071 | 9404.3 | 0.067 |
| Zstandard | `level-15` | 39.779 | 26360 | 47.2 | 0.158 | 11570.5 | 0.413 |
| Zstandard | `level-19` | 42.782 | 24510 | 2.1 | 0.033 | 12069.9 | 0.074 |
| Zstandard | `level-22` | 43.070 | 24346 | 2.0 | 0.006 | 11842.8 | 0.226 |
| Brotli | `q0` | 8.897 | 117852 | 1620.5 | 0.098 | 1157.5 | 0.123 |
| Brotli | `q2` | 13.068 | 80242 | 561.3 | 0.064 | 1475.5 | 0.037 |
| Brotli | `q5` | 26.484 | 39593 | 301.5 | 0.048 | 3126.2 | 0.170 |
| Brotli | `q9` | 36.194 | 28971 | 144.8 | 0.056 | 4594.0 | 0.821 |
| Brotli | `q11` | 40.558 | 25854 | 0.6 | 0.023 | 4905.6 | 0.224 |

### project-high-entropy-medium

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `fast` | 1.000 | 1048674 | 1418.3 | 0.237 | 19166.8 | 0.164 |
| Entroq | `balanced` | 1.000 | 1048674 | 654.2 | 0.165 | 19225.5 | 0.082 |
| LZ4 | `fast-1` | 0.996 | 1052690 | 27746.0 | 0.184 | 58796.5 | 0.633 |
| LZ4 | `fast-3` | 0.996 | 1052690 | 28959.0 | 0.125 | 55069.4 | 0.120 |
| LZ4 | `fast-5` | 0.996 | 1052690 | 26829.5 | 0.543 | 56423.6 | 0.157 |
| LZ4 | `fast-9` | 0.996 | 1052690 | 28793.6 | 0.806 | 56299.4 | 0.074 |
| LZ4 | `hc-1` | 0.996 | 1052690 | 35196.6 | 0.104 | 56426.6 | 0.128 |
| LZ4 | `hc-4` | 0.996 | 1052681 | 59.2 | 0.039 | 43165.5 | 0.033 |
| LZ4 | `hc-9` | 0.996 | 1052681 | 58.6 | 0.044 | 43018.5 | 0.103 |
| LZ4 | `hc-12` | 0.996 | 1052681 | 48.1 | 0.038 | 43165.5 | 0.034 |
| Snappy | `default` | 1.000 | 1048627 | 33915.8 | 0.092 | 66929.0 | 0.154 |
| zlib | `level-1` | 1.000 | 1048896 | 65.7 | 0.014 | 65877.7 | 0.244 |
| zlib | `level-6` | 1.000 | 1048896 | 62.3 | 0.022 | 65364.4 | 0.223 |
| zlib | `level-9` | 1.000 | 1048896 | 61.6 | 0.022 | 62445.0 | 0.055 |
| Zstandard | `level-1` | 1.000 | 1048610 | 8814.7 | 0.194 | 61080.9 | 0.129 |
| Zstandard | `level-3` | 1.000 | 1048609 | 8394.2 | 0.097 | 63072.2 | 0.113 |
| Zstandard | `level-6` | 1.000 | 1048609 | 5061.5 | 0.041 | 68200.1 | 0.125 |
| Zstandard | `level-9` | 1.000 | 1048609 | 4635.5 | 0.216 | 63388.7 | 0.186 |
| Zstandard | `level-12` | 1.000 | 1048609 | 3810.7 | 0.164 | 61680.9 | 0.049 |
| Zstandard | `level-15` | 1.000 | 1048609 | 537.0 | 0.396 | 61830.1 | 0.049 |
| Zstandard | `level-19` | 1.000 | 1048609 | 29.3 | 0.554 | 61230.7 | 0.431 |
| Zstandard | `level-22` | 1.000 | 1048609 | 30.3 | 0.276 | 68570.2 | 0.147 |
| Brotli | `q0` | 1.000 | 1048581 | 10708.8 | 0.035 | 23989.9 | 0.129 |
| Brotli | `q2` | 1.000 | 1048581 | 2390.4 | 0.153 | 25653.2 | 0.061 |
| Brotli | `q5` | 1.000 | 1048581 | 895.5 | 0.168 | 24551.6 | 0.260 |
| Brotli | `q9` | 1.000 | 1048581 | 332.5 | 0.086 | 24268.1 | 0.297 |
| Brotli | `q11` | 1.000 | 1048581 | 4.3 | 0.050 | 25317.5 | 0.137 |

### gutenberg-shakespeare

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `fast` | 2.337 | 2412291 | 73.1 | 0.018 | 153.5 | 0.036 |
| Entroq | `balanced` | 2.685 | 2100016 | 24.8 | 0.062 | 182.0 | 0.036 |
| LZ4 | `fast-1` | 1.598 | 3527975 | 456.6 | 0.041 | 3706.0 | 0.216 |
| LZ4 | `fast-3` | 1.455 | 3874515 | 536.7 | 0.045 | 3761.0 | 0.121 |
| LZ4 | `fast-5` | 1.345 | 4191284 | 617.7 | 0.312 | 3615.7 | 0.141 |
| LZ4 | `fast-9` | 1.236 | 4562723 | 825.3 | 0.106 | 3875.1 | 0.099 |
| LZ4 | `hc-1` | 1.909 | 2953359 | 263.2 | 0.021 | 2939.7 | 0.052 |
| LZ4 | `hc-4` | 2.207 | 2555370 | 69.6 | 0.020 | 3352.8 | 0.130 |
| LZ4 | `hc-9` | 2.283 | 2469328 | 28.1 | 0.019 | 3467.0 | 0.088 |
| LZ4 | `hc-12` | 2.313 | 2438082 | 15.3 | 0.034 | 3403.1 | 0.099 |
| Snappy | `default` | 1.643 | 3430938 | 510.0 | 0.020 | 1848.0 | 0.048 |
| zlib | `level-1` | 2.232 | 2526146 | 122.6 | 0.015 | 363.1 | 0.053 |
| zlib | `level-6` | 2.637 | 2138320 | 24.2 | 0.018 | 378.4 | 0.032 |
| zlib | `level-9` | 2.650 | 2127930 | 18.7 | 0.035 | 384.2 | 0.019 |
| Zstandard | `level-1` | 2.337 | 2412580 | 427.6 | 0.032 | 1457.3 | 0.055 |
| Zstandard | `level-3` | 2.679 | 2104953 | 215.6 | 0.053 | 1236.4 | 0.069 |
| Zstandard | `level-6` | 2.873 | 1962341 | 80.8 | 0.029 | 1252.6 | 0.100 |
| Zstandard | `level-9` | 2.965 | 1901851 | 50.1 | 0.421 | 1313.2 | 0.175 |
| Zstandard | `level-12` | 3.046 | 1850924 | 22.0 | 0.235 | 1367.7 | 0.306 |
| Zstandard | `level-15` | 3.130 | 1801201 | 3.3 | 0.098 | 1495.2 | 0.254 |
| Zstandard | `level-19` | 3.334 | 1691406 | 2.4 | 0.010 | 1438.7 | 0.220 |
| Zstandard | `level-22` | 3.334 | 1691408 | 2.4 | 0.008 | 1430.0 | 0.125 |
| Brotli | `q0` | 2.274 | 2479566 | 304.4 | 0.085 | 302.3 | 0.029 |
| Brotli | `q2` | 2.565 | 2198605 | 122.8 | 0.055 | 359.7 | 0.047 |
| Brotli | `q5` | 2.913 | 1935540 | 43.9 | 0.075 | 422.6 | 0.100 |
| Brotli | `q9` | 3.108 | 1814427 | 14.2 | 0.043 | 505.8 | 0.092 |
| Brotli | `q11` | 3.367 | 1674501 | 0.8 | 0.000 | 502.8 | 0.094 |

### project-logs-large

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `fast` | 3.920 | 2139704 | 118.9 | 0.097 | 249.1 | 0.067 |
| Entroq | `balanced` | 4.891 | 1714939 | 47.8 | 0.062 | 312.6 | 0.069 |
| LZ4 | `fast-1` | 2.874 | 2918436 | 811.2 | 0.083 | 4810.4 | 0.063 |
| LZ4 | `fast-3` | 2.667 | 3144767 | 898.1 | 0.090 | 4911.1 | 0.099 |
| LZ4 | `fast-5` | 2.716 | 3089031 | 914.4 | 0.217 | 4941.6 | 0.107 |
| LZ4 | `fast-9` | 2.509 | 3343188 | 968.8 | 0.070 | 4729.6 | 0.129 |
| LZ4 | `hc-1` | 3.130 | 2680172 | 354.6 | 0.036 | 4131.8 | 0.121 |
| LZ4 | `hc-4` | 3.649 | 2298760 | 121.3 | 0.272 | 4280.1 | 0.143 |
| LZ4 | `hc-9` | 3.855 | 2176141 | 44.4 | 0.047 | 4918.8 | 0.130 |
| LZ4 | `hc-12` | 3.916 | 2142253 | 18.1 | 0.031 | 4314.1 | 0.121 |
| Snappy | `default` | 2.693 | 3114991 | 1037.1 | 0.042 | 3450.3 | 0.016 |
| zlib | `level-1` | 3.699 | 2267654 | 240.3 | 0.041 | 615.3 | 0.050 |
| zlib | `level-6` | 4.793 | 1750314 | 72.0 | 0.023 | 674.6 | 0.021 |
| zlib | `level-9` | 5.018 | 1671738 | 32.8 | 0.031 | 669.9 | 0.038 |
| Zstandard | `level-1` | 4.478 | 1873220 | 640.3 | 0.150 | 1864.7 | 0.101 |
| Zstandard | `level-3` | 4.428 | 1894602 | 417.4 | 0.089 | 1940.1 | 0.124 |
| Zstandard | `level-6` | 5.020 | 1670950 | 117.3 | 0.170 | 2095.6 | 0.226 |
| Zstandard | `level-9` | 5.348 | 1568556 | 70.9 | 0.114 | 2429.2 | 0.205 |
| Zstandard | `level-12` | 5.527 | 1517666 | 35.4 | 0.049 | 2664.6 | 0.160 |
| Zstandard | `level-15` | 5.713 | 1468351 | 6.7 | 0.102 | 2530.1 | 0.273 |
| Zstandard | `level-19` | 6.164 | 1360813 | 2.5 | 0.044 | 2875.7 | 0.295 |
| Zstandard | `level-22` | 6.164 | 1360813 | 2.6 | 0.005 | 2890.7 | 0.324 |
| Brotli | `q0` | 3.664 | 2289283 | 586.3 | 0.048 | 541.2 | 0.037 |
| Brotli | `q2` | 4.297 | 1952286 | 212.1 | 0.083 | 627.2 | 0.057 |
| Brotli | `q5` | 5.236 | 1602023 | 71.0 | 0.034 | 713.1 | 0.121 |
| Brotli | `q9` | 5.556 | 1509696 | 24.6 | 0.051 | 732.1 | 0.235 |
| Brotli | `q11` | 6.337 | 1323781 | 0.8 | 0.000 | 638.4 | 0.097 |


### Reading these tables

The ratios are properties of the bytes. They reproduce exactly, run to run, and the compressed
length beside each one is what the codec actually stored.

The throughputs are not stable in the same way. Measured against the preceding campaign on this
machine, all 1 756 competitor readings that are properties of the bytes agree, and 103 of 550
competitor encode throughputs and 79 of 550 competitor decode throughputs moved outside the
spread the earlier record states, by up to 29 and 38 per cent. Treat a throughput here as the
order of magnitude and the shape of the curve, not as a value to compare against another
published figure. The timing stability section below states what that costs.

### Where Entroq sits, on this machine, at this revision

This is a description of one gate-tier campaign. It is not a frontier claim, and no operating
point here is claimed against any competitor.

**FAST holds a ratio above LZ4 `fast-1` and Snappy on 16 of the 22 entries.** The six
exceptions are the three entries no codec compresses, the two sparse entries, and the tiny run
of zeros, where the run handling of LZ4 and Snappy wins. **BALANCED is at or above FAST on
every entry**: strictly above on the seventeen compressible entries, from 1.0 per cent on
project-mixed-medium to 128 per cent on project-source-medium, and equal on the five
incompressible and RLE entries where both modes store the same bytes.

| Entry | FAST | BALANCED | LZ4 `fast-1` | Snappy | Zstandard `level-1` |
|---|---:|---:|---:|---:|---:|
| project-source-small | 10.719 | 18.094 | 5.587 | 6.861 | 11.130 |
| project-json-medium | 4.863 | 6.521 | 3.641 | 3.397 | 5.595 |
| project-source-medium | 12.233 | 27.881 | 6.026 | 7.196 | 14.677 |
| gutenberg-shakespeare | 2.337 | 2.685 | 1.598 | 1.643 | 2.337 |
| project-logs-large | 3.920 | 4.891 | 2.874 | 2.693 | 4.478 |
| project-high-entropy-medium | 1.000 | 1.000 | 0.996 | 1.000 | 1.000 |

Zstandard `level-1` is the Zstandard level FAST comes closest to, and it reaches a higher ratio
than FAST on four of the five compressible entries above. BALANCED exceeds Zstandard `level-1`
on all five. Every Zstandard level above `level-1` reaches a higher ratio than FAST on all
five, and BALANCED exceeds Zstandard up to `level-6` on project-source-medium.

**FAST encodes six to nine times below LZ4 `fast-1` and Snappy, and BALANCED costs about two to
three times FAST.** On the five compressible entries above the FAST encode gap against LZ4
`fast-1` runs from 6.1 to 6.8 times, and against Snappy from 7.0 to 8.7 times: on
project-source-medium FAST encodes at 288 MB/s against LZ4 `fast-1` at 1784 MB/s and Snappy at
2427 MB/s. BALANCED encodes at 117 MB/s on the same entry.

**Decode is the widest gap, and BALANCED narrows it.** Against Zstandard `level-1`, FAST
decodes between 5.7 and 9.5 times slower on the five compressible entries and BALANCED between
3.7 and 8.0 times slower. BALANCED decodes faster than FAST on every compressible entry:
920 MB/s against 489 MB/s on project-source-medium, and 182 MB/s against 154 MB/s on
gutenberg-shakespeare. This project's stated priority is decode-first, and the better parse
buys decoder throughput as well as ratio: fewer, longer steps to expand.

**Incompressible input costs the parse and nothing after it.** FAST encodes
project-high-entropy-medium at 1418 MB/s and BALANCED at 654 MB/s, where LZ4 `fast-1` reaches
27 746 MB/s and Snappy 33 916 MB/s. A block whose bytes are all equal is stored as RLE without
assembly, and a block the parse finds few matches in is stored as RAW when its literal
distribution leaves no room to code, so such a block never assembles the entropy-coded
candidate streams. It stores the right bytes: the ratio is 1.000 and the frame is 98 bytes
above its content. The parse itself still runs.

**The memory figures are the ones a competitor mostly cannot report.** Entroq states 2 818 164
bytes of encoder steady state under FAST and 3 080 308 under BALANCED, and 327 716 bytes of
decoder steady state, from the machines themselves, at every entry. Of the five competitors,
four publish an encoder state size and one publishes a decoder state size.

**On the ratio-against-encoder-memory chart, BALANCED is not dominated on seven entries.**
The Pareto report over this campaign marks a point dominated when another point for the same
entry is equal or better in compression ratio and in the other axis of the chart, and strictly
better in at least one of them. FAST is dominated on all 22 entries for ratio against encode
throughput, on all 22 for ratio against encoder memory, and on 21 of the 22 for ratio against
decode throughput; the one exception is project-zeros-medium on the decode axis. BALANCED is
dominated on the throughput axes on every compressible entry and stays non-dominated on ratio
against encoder memory on gutenberg-shakespeare, project-large-blob,
project-database-rows-medium, project-json-medium, project-mixed-medium,
project-serialized-binary-medium, and project-source-medium. Where both modes store the same
bytes, FAST dominates BALANCED on the encode and memory axes, and BALANCED is not slower on
decode.

A dominated point is not a rejected one. It can still be the right choice for a property the
axis does not measure, such as a declared memory bound, streaming, random access, or corruption
isolation. This revision claims none of those against a competitor.

## Fairness

A comparison measures equivalent work, or it is not a comparison. Every result states each
axis:

| Axis | Setting |
|---|---|
| input bytes | the same registered entry, by digest, for every codec |
| operating semantics | one-shot compression of a whole buffer, then decompression of it |
| integrity | none, for all six. Each competitor is driven through a format of its own project that carries no checksum. Entroq declares integrity absent, and version 1 of its format defines the position and the width of an integrity field and computes nothing into it. Each result names the checksum it was not produced under |
| resource limits | the library default for each project. Entroq runs at its conservative decoder policy, resource class small, a window of 65 536 bytes |
| threads | 1, except the parallel scaling metric, which states its own worker count |
| warm-up | the same sampling procedure for every codec |
| measured interval | the call alone, inside one process, for every codec |

The integrity axis matters: a codec that checksums less is not faster for free. None of the
six was measured with a checksum enabled, so none is credited or charged for one. Entroq is
the one that has no checksum to enable yet, and that is a gap and not a setting: when the
integrity algorithm exists, the comparison has to be re-read with it on.

The boundary axis matters as much. Entroq is not measured through a thinner interface than
the competitors: every codec here is driven in-process through the API its own project
publishes, and the measured interval is the call and nothing around it.

## Tiers

| Tier | Input budget | Duration | Recorded | What a number licenses |
|---|---|---|---|---|
| `smoke` | 1 MiB | 30 s | no | nothing. It proves the harness runs and produces a parseable result. Its numbers are ignored |
| `dev` | 10 MiB per codec path | 120 s, one segment | yes | direction on one machine, from few samples, at the default operating point of each project. It describes no frontier, and no dev number appears in documentation, in a release note, or in a comparison claim |
| `gate` | 100 MiB per codec path | segmented | yes | a milestone gate. It measures every pinned operating point across the size classes its budget reaches, with repeated samples and a reported spread. The numbers on this page come from this tier. They are scoped to the machine that produced them, and they are not a comparison claim |
| `publication` | full corpora | segmented, authorized | yes | the only tier a number may leave the repository from, and only from a sealed record |

No validation step runs longer than 120 seconds. A campaign that needs more time is split
into segments, not given more time. A segment that overruns is a defect in the run design, and
the budget does not move to accommodate it.

## Records

Every recorded tier writes a manifest of what was requested, an append-only journal with one
line per segment attempt, the raw output of every segment, and a summary written when the
campaign seals. A campaign is sealed when every segment passed, at one revision, against one
recorded input set. Only a sealed record is evidence.

A record holds the command of every segment, so a campaign is reproducible from the record
alone. Records live outside the repository. A run writes nothing inside it.

Three commands read a record:

```sh
entroq-bench report parse   --results <record>                       # read it, whole or not at all
entroq-bench report pareto  --results <record>                       # mark every dominated point
entroq-bench report compare --baseline <record> --results <record>   # did the second reproduce the first
```

`report parse` rejects a document that carries a field it does not declare or omits one it
does, rather than reading it in part. A field skipped is a metric dropped, and a dropped metric
reads as an absent one.

`report compare` reads its tolerance from the first record and chooses none of its own. A
number that is a property of the bytes has no variance, so any difference in it is a finding. A
number that is a timing is judged against the spread the first record states for the block it
came from. A number that moves between runs and that no record states a variance for is
reported with its difference and no verdict.

## Reproducibility

An earlier gate-tier campaign has been reproduced from its own record on a clean checkout, with
the laboratory rebuilt from the pinned commits and the corpus restored from the registry.
Neither the laboratory nor the corpus of the first campaign was reachable from the second.

All 550 competitor rows matched on competitor, operating point, corpus entry, size class, and
thread count, over inputs agreeing by digest, through libraries of the same version. No pair
was rejected as measuring different work, no row existed in one campaign alone, and the two
records disagreed about no field of the host.

The campaign on this page was compared against the preceding gate-tier campaign on the same
host, at two Entroq encoder versions. All 550 competitor rows paired: no pair was rejected as
measuring different work, and the two records disagreed about no field of the host. Every
metric that is a property of the bytes was identical, not within a tolerance: 1 756 such
metrics compared and 0 differ. The Entroq rows do not pair across the two campaigns, because
the earlier campaign measured one operating point and this one measures two; the FAST rows
store the same bytes the earlier point stored, and the BALANCED rows are measured here for the
first time.

The ratios beside the Entroq rows are the ones this encoder version writes. A later encoder
version is free to change them without changing what a decoder accepts.

## Timing stability

Throughput did not reproduce inside the spread a record states. This is a property of the host,
and it is stated here so that no future number from an uncontrolled machine is read as more
stable than it is.

Between the two campaigns described above, 103 of the 550 competitor encode throughputs and 79
of the 550 competitor decode throughputs moved outside the spread the earlier record states.
The largest movement was 29 per cent on an encode throughput and 38 per cent on a decode
throughput. Those rows ran the same library over the same bytes at the same operating point, so
the movement is the machine and nothing else.

The spread a result reports is taken from samples seconds apart inside one segment, under one
machine state. It does not bound the movement between runs on different days, and the gap
between the two grows with the cost of the operating point. The host publishes no governor,
boost, or thermal state a run can read or control.

This is why the numbers on this page carry their spread and their machine, and why none of
them is stated as a comparison claim. A claim that one codec is faster than another needs a
controlled machine and a publication-tier record. Neither exists yet.

## What this laboratory does not answer

```text
GAP: no host this project reaches grants a cycle count or an instruction count.

Known:
- The performance monitor registers trap at the user exception level on the
  development host. Measured.
- `perf_event_open` is a Linux call and does not exist on that host. A shared
  runner usually denies the counter.

Unknown:
- Whether any host this project can reach grants counter access.

Blocks:
- The three counter metrics, on every host this project has.
- The two frontier axes read against encode CPU and decode CPU. Both report no
  data rather than substituting elapsed time.
```

```text
GAP: the decode throughput is below every competitor at a comparable ratio.

Known:
- Measured above, on the five compressible entries tabled: FAST decodes
  between 5.7 and 9.5 times below Zstandard level-1 at a ratio at or below
  Zstandard level-1's own, and BALANCED between 3.7 and 8.0 times below it at
  a ratio above Zstandard level-1's own on all five.
- Nothing in the format requires it. This revision's decoder allocates a
  symbol vector per stream, per block.
- Decode-first is a stated project priority, so this is the figure furthest
  from where it needs to be.

Unknown:
- How much of the gap the per-block allocations account for, and how much is
  the decomposition, the table shape, or the sequence loop.

Blocks:
- Any operating point being claimed against a competitor on a decode axis.
```

```text
GAP: the encoder still parses a block it will store raw.

Known:
- Measured above: FAST at 1 418 MB/s and BALANCED at 654 MB/s on
  incompressible input, against 27 746 MB/s for LZ4 fast-1 and 33 916 MB/s
  for Snappy, a gap of about 20 times for FAST.
- A block the parse finds few matches in is stored as RAW without assembling
  the entropy-coded candidate streams, and a block whose bytes are all equal
  is stored as RLE without assembly. The parse itself runs either way, so the
  match search is still paid on a block that stores no match.
- The bytes are right: the ratio is 1.000 and the frame is 98 bytes above its
  content.

Unknown:
- What a decision taken before the parse would cost in ratio on content that
  is nearly incompressible rather than incompressible.

Blocks:
- Any operating point being claimed against a competitor on an
  already-compressed corpus entry.
```

```text
GAP: the two operating points this revision exposes are dominated on the throughput frontiers.

Known:
- Measured above, by the Pareto report over this campaign: FAST is dominated on 22 of 22
  entries for ratio against encode throughput and for ratio against encoder memory, and on
  21 of 22 for ratio against decode throughput. BALANCED is dominated on the throughput axes
  on every compressible entry, and non-dominated on ratio against encoder memory on seven.
- The FAST encoder steady state is 2 818 164 bytes and the BALANCED one is 3 080 308, against
  582 680 for Zstandard level-1 and 262 200 for LZ4 hc-4, both of which also reach a higher
  ratio than FAST on project-source-medium.

Unknown:
- Which axis the points should be moved along first, and what a further mechanism would buy
  on an axis this report does not chart.

Blocks:
- Any statement that either operating point sits on the throughput frontier.
```

```text
GAP: no publication-tier record exists.

Known:
- Every campaign so far is dev tier or gate tier. This includes every Entroq
  number on this page.
- A publication campaign needs a controlled machine and an authorization, and
  has had neither.
- The numbers on this page are gate tier, published with their scope and their
  spread, and stated as measurements of this machine rather than as claims.

Blocks:
- Any comparison claim, and any number quoted without the machine that
  produced it.
```

```text
GAP: throughput at an expensive operating point does not repeat on the
development host.

Known:
- Stated under Timing stability above.
- The host publishes no governor, boost, or thermal state a run can read or
  control, and every result records that.

Blocks:
- Reading a gate-tier throughput at an expensive operating point as a stable
  value.
- Any published comparison, until a controlled machine measures it.
```

```text
GAP: random-range latency and range-read amplification have no producer.

Known:
- No codec path in this repository produces a range read.
- No competitor measured here publishes one either.

Blocks:
- Both metrics. They wait for the Entroq index and the range decode path.
```

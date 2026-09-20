---
title: Benchmarks
description: What Entroq measures, against what, how, and what a number from each tier licenses.
class: reference
audience: someone reading, reproducing, or citing an Entroq measurement
order: 2
version_axes: [encoder_version, harness_version]
encoder_version: 0.0.0. One encode path, no mode, a window of 65 536 bytes and a block of 65 536 input bytes.
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
Entroq exposes one, because this revision admits no mode: it ships one encode path, with one
match finder, one parser, one representation, and one block length.

| Codec | Points | Groups |
|---|---|---|
| Entroq | `default` | `default` |
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

Source: a gate-tier campaign sealed at one revision, 48 of 48 segments, 501 seconds. It holds
572 measured rows over 22 corpus entries: 22 Entroq rows, one per entry at the one operating
point this revision exposes, and 550 competitor rows. The six tables below are the entries
that show the most about codec behavior; the campaign measured the rest.

Every row of every table shares this scope:

```text
machine       Apple M1 Pro, 10 cores, 16 GiB, macOS Darwin 25.5.0, aarch64
build         release profile, rustc 1.98.1
Entroq        encoder version 0.0.0. One frame, resource class small, regions independent,
              a window of 65 536 bytes, a region of 1 048 576 input bytes, a block of
              65 536 input bytes. A single-entry match table of 16 384 entries behind a tag
              gate, an adaptive skip over searched misses, and a greedy parse
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

Source text at the small class. Small class, 32768 bytes.

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `default` | 10.719 | 3057 | 313.8 | 0.046 | 562.9 | 0.057 |
| LZ4 | `fast-1` | 5.587 | 5865 | 1998.8 | 0.005 | 7178.1 | 0.018 |
| LZ4 | `fast-3` | 5.599 | 5852 | 2041.2 | 0.128 | 7383.5 | 0.054 |
| LZ4 | `fast-5` | 5.496 | 5962 | 2057.3 | 0.018 | 7425.3 | 0.010 |
| LZ4 | `fast-9` | 5.351 | 6124 | 2046.2 | 0.021 | 7435.4 | 0.023 |
| LZ4 | `hc-1` | 8.478 | 3865 | 2201.3 | 0.025 | 11736.4 | 0.025 |
| LZ4 | `hc-4` | 14.397 | 2276 | 534.9 | 0.048 | 21099.8 | 0.050 |
| LZ4 | `hc-9` | 14.888 | 2201 | 403.0 | 0.123 | 21801.7 | 0.023 |
| LZ4 | `hc-12` | 15.170 | 2160 | 50.1 | 0.025 | 22459.2 | 0.024 |
| Snappy | `default` | 6.861 | 4776 | 2581.0 | 0.064 | 6696.9 | 0.073 |
| zlib | `level-1` | 10.304 | 3180 | 962.3 | 0.159 | 1760.7 | 0.212 |
| zlib | `level-6` | 19.692 | 1664 | 279.9 | 0.184 | 2410.8 | 0.237 |
| zlib | `level-9` | 19.859 | 1650 | 181.0 | 0.272 | 2426.9 | 0.152 |
| Zstandard | `level-1` | 11.130 | 2944 | 1080.5 | 0.018 | 2935.7 | 0.031 |
| Zstandard | `level-3` | 14.309 | 2290 | 1508.8 | 0.037 | 3970.0 | 0.016 |
| Zstandard | `level-6` | 16.744 | 1957 | 333.7 | 0.214 | 4652.6 | 0.030 |
| Zstandard | `level-9` | 19.230 | 1704 | 215.1 | 0.101 | 5305.7 | 0.014 |
| Zstandard | `level-12` | 19.692 | 1664 | 72.4 | 0.046 | 6055.8 | 0.009 |
| Zstandard | `level-15` | 20.818 | 1574 | 10.8 | 0.010 | 5496.1 | 0.096 |
| Zstandard | `level-19` | 21.347 | 1535 | 1.9 | 0.019 | 5523.0 | 0.022 |
| Zstandard | `level-22` | 21.347 | 1535 | 1.8 | 0.094 | 5200.4 | 0.107 |
| Brotli | `q0` | 8.219 | 3987 | 1265.9 | 0.135 | 1132.7 | 0.058 |
| Brotli | `q2` | 11.405 | 2873 | 512.6 | 0.198 | 1471.1 | 0.304 |
| Brotli | `q5` | 19.219 | 1705 | 244.6 | 0.326 | 2699.8 | 0.301 |
| Brotli | `q9` | 21.154 | 1549 | 240.4 | 0.142 | 3025.7 | 0.138 |
| Brotli | `q11` | 22.291 | 1470 | 0.7 | 0.059 | 2696.5 | 0.016 |

### project-json-medium

JSON records, generated from a recorded seed. Medium class, 262144 bytes.

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `default` | 4.863 | 53905 | 146.1 | 0.068 | 287.8 | 0.058 |
| LZ4 | `fast-1` | 3.641 | 71999 | 977.4 | 0.093 | 4734.0 | 0.029 |
| LZ4 | `fast-3` | 3.455 | 75871 | 944.7 | 0.132 | 4762.7 | 0.104 |
| LZ4 | `fast-5` | 3.396 | 77203 | 1023.5 | 0.082 | 4702.1 | 0.072 |
| LZ4 | `fast-9` | 3.292 | 79641 | 983.5 | 0.051 | 4723.3 | 0.078 |
| LZ4 | `hc-1` | 3.886 | 67466 | 555.6 | 0.125 | 4653.5 | 0.109 |
| LZ4 | `hc-4` | 4.880 | 53723 | 165.9 | 0.112 | 5693.7 | 0.068 |
| LZ4 | `hc-9` | 5.330 | 49180 | 50.6 | 0.082 | 6838.6 | 0.073 |
| LZ4 | `hc-12` | 5.412 | 48438 | 18.5 | 0.026 | 6615.6 | 0.096 |
| Snappy | `default` | 3.397 | 77166 | 1206.0 | 0.057 | 3917.4 | 0.003 |
| zlib | `level-1` | 4.801 | 54601 | 308.1 | 0.142 | 780.3 | 0.320 |
| zlib | `level-6` | 6.515 | 40236 | 93.6 | 0.126 | 911.9 | 0.267 |
| zlib | `level-9` | 6.831 | 38376 | 33.2 | 0.058 | 911.8 | 0.208 |
| Zstandard | `level-1` | 5.595 | 46852 | 668.5 | 0.042 | 1902.5 | 0.726 |
| Zstandard | `level-3` | 5.478 | 47853 | 560.5 | 0.057 | 2081.9 | 0.096 |
| Zstandard | `level-6` | 6.423 | 40816 | 143.9 | 0.138 | 2515.6 | 0.083 |
| Zstandard | `level-9` | 7.068 | 37089 | 76.3 | 0.038 | 2868.9 | 0.075 |
| Zstandard | `level-12` | 7.531 | 34809 | 22.3 | 0.163 | 3130.1 | 0.076 |
| Zstandard | `level-15` | 7.932 | 33049 | 7.2 | 0.129 | 3036.4 | 0.121 |
| Zstandard | `level-19` | 7.995 | 32787 | 3.5 | 0.155 | 3054.1 | 0.100 |
| Zstandard | `level-22` | 7.995 | 32787 | 2.4 | 0.124 | 2911.4 | 0.077 |
| Brotli | `q0` | 4.313 | 60785 | 778.9 | 0.372 | 668.2 | 0.180 |
| Brotli | `q2` | 5.575 | 47018 | 279.4 | 0.177 | 726.6 | 0.143 |
| Brotli | `q5` | 6.706 | 39092 | 89.3 | 0.185 | 882.4 | 0.193 |
| Brotli | `q9` | 7.229 | 36263 | 51.3 | 0.122 | 914.1 | 0.379 |
| Brotli | `q11` | 8.416 | 31147 | 0.9 | 0.047 | 826.4 | 0.058 |

### project-source-medium

Rust and C source text, generated from a recorded seed. Medium class, 1048576 bytes.

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `default` | 12.233 | 85717 | 305.3 | 0.073 | 484.3 | 0.096 |
| LZ4 | `fast-1` | 6.026 | 174021 | 1875.8 | 0.031 | 6180.2 | 0.033 |
| LZ4 | `fast-3` | 6.015 | 174315 | 1884.7 | 0.023 | 6244.6 | 0.044 |
| LZ4 | `fast-5` | 6.005 | 174622 | 1881.1 | 0.018 | 6258.6 | 0.032 |
| LZ4 | `fast-9` | 6.000 | 174773 | 1879.4 | 0.025 | 6260.2 | 0.027 |
| LZ4 | `hc-1` | 9.422 | 111293 | 2298.9 | 0.056 | 8009.5 | 0.051 |
| LZ4 | `hc-4` | 20.108 | 52146 | 427.8 | 0.094 | 13294.1 | 0.092 |
| LZ4 | `hc-9` | 24.835 | 42221 | 145.3 | 0.034 | 16416.1 | 0.068 |
| LZ4 | `hc-12` | 26.209 | 40008 | 39.4 | 0.032 | 15354.3 | 0.109 |
| Snappy | `default` | 7.196 | 145711 | 2453.3 | 0.275 | 6563.9 | 0.014 |
| zlib | `level-1` | 11.528 | 90957 | 629.4 | 0.102 | 1453.0 | 0.087 |
| zlib | `level-6` | 29.997 | 34956 | 239.8 | 0.043 | 2532.8 | 0.039 |
| zlib | `level-9` | 31.104 | 33712 | 127.3 | 0.064 | 2534.8 | 0.197 |
| Zstandard | `level-1` | 14.677 | 71445 | 1570.2 | 0.092 | 3682.4 | 0.057 |
| Zstandard | `level-3` | 17.101 | 61318 | 1704.4 | 0.295 | 4531.1 | 0.095 |
| Zstandard | `level-6` | 23.171 | 45253 | 320.4 | 0.098 | 6410.1 | 0.042 |
| Zstandard | `level-9` | 28.063 | 37365 | 243.2 | 0.096 | 8416.7 | 0.078 |
| Zstandard | `level-12` | 33.152 | 31629 | 140.6 | 0.085 | 9482.3 | 0.076 |
| Zstandard | `level-15` | 39.779 | 26360 | 48.1 | 0.170 | 12234.3 | 0.381 |
| Zstandard | `level-19` | 42.782 | 24510 | 2.2 | 0.041 | 11732.3 | 0.124 |
| Zstandard | `level-22` | 43.070 | 24346 | 2.1 | 0.064 | 12646.2 | 0.116 |
| Brotli | `q0` | 8.897 | 117852 | 1656.3 | 0.090 | 1213.9 | 0.091 |
| Brotli | `q2` | 13.068 | 80242 | 536.4 | 0.127 | 1455.7 | 0.254 |
| Brotli | `q5` | 26.484 | 39593 | 288.4 | 0.092 | 3087.1 | 0.126 |
| Brotli | `q9` | 36.194 | 28971 | 141.9 | 0.073 | 4814.6 | 0.628 |
| Brotli | `q11` | 40.558 | 25854 | 0.6 | 0.006 | 5318.2 | 0.050 |

### project-high-entropy-medium

Incompressible bytes, generated from a recorded seed. Medium class, 1048576 bytes.

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `default` | 1.000 | 1048674 | 1429.9 | 0.051 | 24892.0 | 0.139 |
| LZ4 | `fast-1` | 0.996 | 1052690 | 31068.9 | 0.177 | 59211.5 | 0.174 |
| LZ4 | `fast-3` | 0.996 | 1052690 | 29262.0 | 0.126 | 57065.4 | 0.150 |
| LZ4 | `fast-5` | 0.996 | 1052690 | 32514.0 | 0.234 | 59352.2 | 0.165 |
| LZ4 | `fast-9` | 0.996 | 1052690 | 31935.7 | 0.174 | 59211.5 | 0.153 |
| LZ4 | `hc-1` | 0.996 | 1052690 | 31222.5 | 0.218 | 57456.2 | 0.139 |
| LZ4 | `hc-4` | 0.996 | 1052681 | 58.9 | 0.075 | 43240.2 | 0.024 |
| LZ4 | `hc-9` | 0.996 | 1052681 | 59.4 | 0.065 | 43240.2 | 0.033 |
| LZ4 | `hc-12` | 0.996 | 1052681 | 50.3 | 0.049 | 43240.2 | 0.014 |
| Snappy | `default` | 1.000 | 1048627 | 30283.8 | 0.290 | 60349.7 | 0.098 |
| zlib | `level-1` | 1.000 | 1048896 | 65.3 | 0.032 | 62137.8 | 0.057 |
| zlib | `level-6` | 1.000 | 1048896 | 61.6 | 0.046 | 58390.5 | 0.146 |
| zlib | `level-9` | 1.000 | 1048896 | 60.2 | 0.032 | 61230.7 | 0.139 |
| Zstandard | `level-1` | 1.000 | 1048610 | 9645.7 | 0.223 | 65197.8 | 0.119 |
| Zstandard | `level-3` | 1.000 | 1048609 | 8654.0 | 0.129 | 60349.7 | 0.084 |
| Zstandard | `level-6` | 1.000 | 1048609 | 4969.6 | 0.039 | 66576.3 | 0.124 |
| Zstandard | `level-9` | 1.000 | 1048609 | 4572.3 | 0.241 | 61983.6 | 0.214 |
| Zstandard | `level-12` | 1.000 | 1048609 | 4341.2 | 0.234 | 66223.1 | 0.266 |
| Zstandard | `level-15` | 1.000 | 1048609 | 530.0 | 0.336 | 64691.0 | 0.121 |
| Zstandard | `level-19` | 1.000 | 1048609 | 38.8 | 0.214 | 65536.0 | 0.091 |
| Zstandard | `level-22` | 1.000 | 1048609 | 34.8 | 0.296 | 65536.0 | 0.115 |
| Brotli | `q0` | 1.000 | 1048581 | 10352.0 | 0.079 | 24916.9 | 0.161 |
| Brotli | `q2` | 1.000 | 1048581 | 2459.8 | 0.167 | 22469.3 | 0.088 |
| Brotli | `q5` | 1.000 | 1048581 | 878.7 | 0.233 | 21694.8 | 0.403 |
| Brotli | `q9` | 1.000 | 1048581 | 318.1 | 0.156 | 23989.9 | 0.180 |
| Brotli | `q11` | 1.000 | 1048581 | 4.4 | 0.123 | 25165.6 | 0.032 |

### gutenberg-shakespeare

English literary text, the complete works of Shakespeare. Large class, 5638480 bytes.

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `default` | 2.337 | 2412291 | 74.9 | 0.027 | 157.6 | 0.027 |
| LZ4 | `fast-1` | 1.598 | 3527975 | 472.2 | 0.034 | 3944.9 | 0.012 |
| LZ4 | `fast-3` | 1.455 | 3874515 | 563.1 | 0.031 | 3959.8 | 0.025 |
| LZ4 | `fast-5` | 1.345 | 4191284 | 669.7 | 0.039 | 3868.3 | 0.048 |
| LZ4 | `fast-9` | 1.236 | 4562723 | 857.2 | 0.077 | 4035.2 | 0.013 |
| LZ4 | `hc-1` | 1.909 | 2953359 | 255.4 | 0.056 | 2903.6 | 0.056 |
| LZ4 | `hc-4` | 2.207 | 2555370 | 69.2 | 0.106 | 3198.6 | 0.093 |
| LZ4 | `hc-9` | 2.283 | 2469328 | 28.7 | 0.034 | 3481.3 | 0.184 |
| LZ4 | `hc-12` | 2.313 | 2438082 | 15.6 | 0.100 | 3420.1 | 0.084 |
| Snappy | `default` | 1.643 | 3430938 | 507.6 | 0.024 | 1886.0 | 0.056 |
| zlib | `level-1` | 2.232 | 2526146 | 122.9 | 0.022 | 363.8 | 0.062 |
| zlib | `level-6` | 2.637 | 2138320 | 24.0 | 0.011 | 378.5 | 0.030 |
| zlib | `level-9` | 2.650 | 2127930 | 18.5 | 0.023 | 379.6 | 0.035 |
| Zstandard | `level-1` | 2.337 | 2412580 | 439.9 | 0.045 | 1498.1 | 0.007 |
| Zstandard | `level-3` | 2.679 | 2104953 | 225.8 | 0.131 | 1271.0 | 0.063 |
| Zstandard | `level-6` | 2.873 | 1962341 | 85.1 | 0.038 | 1249.2 | 0.096 |
| Zstandard | `level-9` | 2.965 | 1901851 | 51.9 | 0.123 | 1439.2 | 0.061 |
| Zstandard | `level-12` | 3.046 | 1850924 | 22.1 | 0.197 | 1539.8 | 0.043 |
| Zstandard | `level-15` | 3.130 | 1801201 | 3.6 | 0.098 | 1421.7 | 0.212 |
| Zstandard | `level-19` | 3.334 | 1691406 | 2.4 | 0.113 | 1393.1 | 0.207 |
| Zstandard | `level-22` | 3.334 | 1691408 | 2.1 | 0.046 | 1453.7 | 0.199 |
| Brotli | `q0` | 2.274 | 2479566 | 295.1 | 0.029 | 292.5 | 0.032 |
| Brotli | `q2` | 2.565 | 2198605 | 119.6 | 0.061 | 349.3 | 0.042 |
| Brotli | `q5` | 2.913 | 1935540 | 48.2 | 0.062 | 386.3 | 0.163 |
| Brotli | `q9` | 3.108 | 1814427 | 14.7 | 0.159 | 500.7 | 0.087 |
| Brotli | `q11` | 3.367 | 1674501 | 0.7 | 0.000 | 493.6 | 0.146 |

### project-logs-large

Line-oriented application logs, generated from a recorded seed. Large class, 8388608 bytes.

| Codec | Point | Ratio | Compressed bytes | Encode MB/s | Encode spread | Decode MB/s | Decode spread |
|---|---|---:|---:|---:|---:|---:|---:|
| Entroq | `default` | 3.920 | 2139704 | 127.7 | 0.019 | 262.1 | 0.026 |
| LZ4 | `fast-1` | 2.874 | 2918436 | 856.8 | 0.010 | 5091.2 | 0.022 |
| LZ4 | `fast-3` | 2.667 | 3144767 | 952.4 | 0.036 | 5161.8 | 0.016 |
| LZ4 | `fast-5` | 2.716 | 3089031 | 977.4 | 0.048 | 5052.1 | 0.109 |
| LZ4 | `fast-9` | 2.509 | 3343188 | 1028.5 | 0.029 | 4979.6 | 0.056 |
| LZ4 | `hc-1` | 3.130 | 2680172 | 371.2 | 0.040 | 4296.0 | 0.066 |
| LZ4 | `hc-4` | 3.649 | 2298760 | 126.2 | 0.016 | 4425.4 | 0.009 |
| LZ4 | `hc-9` | 3.855 | 2176141 | 46.1 | 0.027 | 4988.5 | 0.092 |
| LZ4 | `hc-12` | 3.916 | 2142253 | 18.7 | 0.011 | 4335.9 | 0.010 |
| Snappy | `default` | 2.693 | 3114991 | 996.3 | 0.041 | 3402.9 | 0.082 |
| zlib | `level-1` | 3.699 | 2267654 | 237.0 | 0.061 | 605.1 | 0.060 |
| zlib | `level-6` | 4.793 | 1750314 | 71.5 | 0.014 | 658.7 | 0.049 |
| zlib | `level-9` | 5.018 | 1671738 | 32.6 | 0.018 | 669.2 | 0.041 |
| Zstandard | `level-1` | 4.478 | 1873220 | 664.5 | 0.020 | 1889.3 | 0.061 |
| Zstandard | `level-3` | 4.428 | 1894602 | 434.3 | 0.077 | 1998.7 | 0.067 |
| Zstandard | `level-6` | 5.020 | 1670950 | 130.5 | 0.109 | 2321.6 | 0.064 |
| Zstandard | `level-9` | 5.348 | 1568556 | 76.8 | 0.206 | 2360.9 | 0.422 |
| Zstandard | `level-12` | 5.527 | 1517666 | 36.2 | 0.050 | 2510.6 | 0.138 |
| Zstandard | `level-15` | 5.713 | 1468351 | 7.3 | 0.038 | 2677.6 | 0.442 |
| Zstandard | `level-19` | 6.164 | 1360813 | 2.6 | 0.010 | 2853.5 | 0.373 |
| Zstandard | `level-22` | 6.164 | 1360813 | 2.7 | 0.023 | 2637.0 | 0.217 |
| Brotli | `q0` | 3.664 | 2289283 | 573.4 | 0.096 | 528.1 | 0.029 |
| Brotli | `q2` | 4.297 | 1952286 | 203.8 | 0.073 | 618.7 | 0.079 |
| Brotli | `q5` | 5.236 | 1602023 | 74.4 | 0.055 | 673.2 | 0.140 |
| Brotli | `q9` | 5.556 | 1509696 | 24.7 | 0.394 | 746.3 | 0.092 |
| Brotli | `q11` | 6.337 | 1323781 | 0.8 | 0.000 | 655.0 | 0.171 |

### Reading these tables

The ratios are properties of the bytes. They reproduce exactly, run to run, and the compressed
length beside each one is what the codec actually stored.

The throughputs are not stable in the same way. Measured against an earlier campaign on this
machine, 77 of 550 competitor encode throughputs and 72 of 550 competitor decode throughputs
moved outside the spread the earlier record states, by up to 46 and 66 per cent. Treat a
throughput here as the order of magnitude and the shape of the curve, not as a value to compare
against another published figure. The timing stability section below states what that costs.

### Where Entroq sits, on this machine, at this revision

This is a description of one gate-tier campaign. It is not a frontier claim, and no operating
point here is claimed against any competitor.

**The ratio is above LZ4 `fast-1` and Snappy on 16 of the 22 entries.** The six exceptions are
the three entries no codec compresses, the two sparse entries, and the tiny run of zeros, where
the run handling of LZ4 and Snappy wins.

| Entry | Entroq | LZ4 `fast-1` | Snappy | Zstandard `level-1` |
|---|---:|---:|---:|---:|
| project-source-small | 10.719 | 5.587 | 6.861 | 11.130 |
| project-json-medium | 4.863 | 3.641 | 3.397 | 5.595 |
| project-source-medium | 12.233 | 6.026 | 7.196 | 14.677 |
| gutenberg-shakespeare | 2.337 | 1.598 | 1.643 | 2.337 |
| project-logs-large | 3.920 | 2.874 | 2.693 | 4.478 |
| project-high-entropy-medium | 1.000 | 0.996 | 1.000 | 1.000 |

Zstandard `level-1` is the Zstandard level Entroq comes closest to, and it reaches a higher
ratio on four of the five compressible entries above. On gutenberg-shakespeare the two ratios
agree to three decimals and Entroq stores 2 412 291 bytes against 2 412 580, 289 bytes fewer.
Every Zstandard level above `level-1` reaches a higher ratio than Entroq on all five.

**Encode is six to eight times below LZ4 `fast-1` and Snappy.** On the five compressible
entries above the encode gap against LZ4 `fast-1` runs from 6.1 to 6.7 times, and against
Snappy from 6.8 to 8.3 times. On project-source-medium Entroq encodes at 305 MB/s against LZ4
`fast-1` at 1876 MB/s and Snappy at 2453 MB/s.

**Decode is the widest gap.** Against Zstandard `level-1`, whose ratio Entroq reaches between
0.83 and 1.00 times on these entries, Entroq decodes between 5.2 and 9.5 times slower on the
five compressible entries: 484 MB/s against 3682 MB/s on project-source-medium, and 158 MB/s
against 1498 MB/s on gutenberg-shakespeare. Against LZ4 `fast-1` the gap runs from 12.8 to
25.0 times. This project's stated priority is decode-first, so this is the number furthest
from where it needs to be. Nothing in the format requires it.

**Incompressible input costs the parse and nothing after it.** Entroq encodes
project-high-entropy-medium at 1430 MB/s, where LZ4 `fast-1` reaches 31 069 MB/s and Snappy
30 284 MB/s, a gap of about 21 times. A block whose bytes are all equal is stored as RLE
without assembly, and a block the parse finds few matches in is stored as RAW when its literal
distribution leaves no room to code, so such a block never assembles the entropy-coded
candidate streams. It stores the right bytes: the ratio is 1.000 and the frame is 98 bytes
above its content. The parse itself still runs.

**The memory figures are the ones a competitor mostly cannot report.** Entroq states 2 818 164
bytes of encoder steady state and 327 716 bytes of decoder steady state, from the machines
themselves, at every entry. Of the five competitors, four publish an encoder state size and one
publishes a decoder state size.

**Every point this revision exposes is dominated.** The Pareto report over this campaign marks
a point dominated when another point for the same entry is equal or better in compression ratio
and in the other axis of the chart, and strictly better in at least one of them. Entroq's
single point is dominated on all 22 entries for ratio against encode throughput, on all 22 for
ratio against encoder memory, and on 21 of the 22 for ratio against decode throughput. The one
exception is project-zeros-medium on the decode axis.

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
host, two days apart, at two Entroq revisions. All 572 rows paired: no row existed in one
campaign alone, no pair was rejected as measuring different work, and the two records disagreed
about no field of the host.

Across the 550 competitor rows every metric that is a property of the bytes was identical, not
within a tolerance: 1 756 such metrics compared and 0 differ. Across the 22 Entroq rows the
encoder state size and the decoder state size were identical on every row, and 10 of the 22
compressed lengths were identical. The other 12 changed, because the encoder changed between
the two revisions.

The ratios beside the Entroq rows are the ones this encoder version writes. A later encoder
version is free to change them without changing what a decoder accepts.

## Timing stability

Throughput did not reproduce inside the spread a record states. This is a property of the host,
and it is stated here so that no future number from an uncontrolled machine is read as more
stable than it is.

Between the two campaigns described above, 77 of the 550 competitor encode throughputs and 72
of the 550 competitor decode throughputs moved outside the spread the earlier record states.
The largest movement was 46 per cent on an encode throughput and 66 per cent on a decode
throughput. Those rows ran the same library over the same bytes at the same operating point, so
the movement is the machine and nothing else.

The Entroq rows moved further, and their movement is not a noise figure: 15 of the 22 decode
throughputs fell outside the earlier spread, and the decoder changed between the two revisions,
so that figure mixes a code change with the host.

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
GAP: the decode throughput is far below every competitor at a comparable
ratio.

Known:
- Measured above, on the five compressible entries tabled: between 5.2 and
  9.5 times below Zstandard level-1, at a ratio at or below Zstandard
  level-1's own, and between 12.8 and 25.0 times below LZ4 fast-1.
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
- Measured above: 1 430 MB/s on incompressible input, against 31 069 MB/s for
  LZ4 fast-1 and 30 284 MB/s for Snappy, a gap of about 21 times.
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
GAP: the one operating point this revision exposes is dominated.

Known:
- Measured above, by the Pareto report over this campaign: dominated on 22 of
  22 entries for ratio against encode throughput, 22 of 22 for ratio against
  encoder memory, and 21 of 22 for ratio against decode throughput.
- The encoder steady state is 2 818 164 bytes, against 582 680 for Zstandard
  level-1 and 262 200 for LZ4 hc-4, both of which also reach a higher ratio on
  project-source-medium.

Unknown:
- Which axis the point should be moved along first, and what a mode that
  declared a memory bound would buy on an axis this report does not chart.

Blocks:
- Any statement that this operating point sits on a frontier.
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

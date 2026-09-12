# Entroq

Entroq is a general-purpose lossless compression format and codec, written in Rust.

Entroq is a new format. It is not a Rust rewrite of Zstandard, LZ4, Brotli, Snappy, or
Deflate, and it is not bitstream compatible with any of them.

## Status

Early development.

The codec crate exports a format module and a streaming module. Between them they hold the
frame, region and block structure, a streaming encoder, and a streaming decoder. Every other
module in the crate is private, because the mechanism it owns has not been designed and
measured. There is no Entroq command line tool.

The structure carries no compression. Content travels literally or as a run, so a stream is
a validated container and not yet a compressed representation. There is nothing to compress
with yet.

Beyond that, the repository holds the measurement foundation the codec is built against:

* A validation runner. Four tiers, a hard 120 second budget per segment, an append-only
  record per run, resume, and sealing.
* A competitor laboratory builder. It builds LZ4, Zstandard, Brotli, Snappy, and zlib from
  pinned upstream commits, and records how each build was produced.
* A corpus registry. It generates the project corpus from recorded seeds and fetches every
  registered public corpus against a pinned checksum. No corpus byte is committed.
* A benchmark harness. It measures a competitor in-process, through the library the
  laboratory built, and writes one machine-readable result per segment.
* A result parser, a Pareto report, and a comparison that states whether one recorded
  campaign reproduced another.
* Fuzz infrastructure. Eleven targets are declared against the parsers and decoders Entroq
  will have. Two carry a driver, for the frame header parser and for the streaming decoder;
  the other nine wait for the code they cover. A twelfth target exercises the fuzz runner
  itself and covers no Entroq code.
* An integration lane that runs the gates on a clean Linux host, in a container.

Every number these produce is a competitor number. No Entroq number exists, and every
result says so rather than carrying a zero.

The next section states goals. Read no sentence in it as current behavior.

## Goals

* Exactness. `decode(encode(x)) == x` for every input, every mode, and every chunking.
* Bounded memory. Every mode declares a memory bound and stays within it.
* Streaming as the base execution model, not a wrapper over a one-shot path.
* A decoder that inspects the requirements a stream declares, and rejects the stream
  before it allocates for it.
* Parallelism enabled by the format, not simulated around a serial stream.
* Random access as a first-class feature.
* Safe behavior on malformed input. No panic across the public API, no memory unsafety,
  no unbounded allocation, and no infinite loop.
* Reproducible benchmark numbers, from recorded inputs on a recorded machine.

Entroq will expose a small number of intentional modes, not dozens of numeric levels.

## Build

`rust-toolchain.toml` pins the toolchain. `rustup` reads that file and installs the
pinned channel on the first build.

Every action enters through the `Makefile`. `make help` lists every target.

```sh
make build      # compile the workspace
make check      # type check the workspace
make validate   # format check, lint, and the dev validation tier
make ci         # run the gates on a clean Linux host, in a container
```

Building and validating needs the pinned toolchain and nothing else. The laboratory and the
corpus are build inputs that live outside the repository, and only the benchmark targets
read them. `CONTRIBUTING.md` states how to produce both.

## Documentation

`docs/` holds the user guide, the technical documentation, and the benchmark documentation.
`docs/README.md` lists every page, and names the pages that do not exist yet because the
behavior they would state does not exist.

`docs/benchmarks.md` states what Entroq measures, against what, how, and what a number from
each tier licenses.

`docs/format.md` states the format contract: the frame, region and block structure, and
what a conforming decoder accepts, refuses, and ignores. A third party must be able to write
a conforming decoder from that page alone. The format is implemented and not frozen, and the
page marks every field that is still provisional.

## Contributing

`CONTRIBUTING.md` states how to build a change, validate it, and submit it.

`CODE_OF_CONDUCT.md` applies to every space of this project.

## License

Copyright 2026 Diego Martín Lafuente <meerita@icloud.com>

Licensed under either of:

* Apache License, Version 2.0 (`LICENSE-APACHE`, or
  http://www.apache.org/licenses/LICENSE-2.0)
* MIT license (`LICENSE-MIT`, or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you state otherwise, you intentionally submit any contribution for inclusion in
Entroq, as defined in the Apache-2.0 license, dual licensed as above, without any
additional term or condition.

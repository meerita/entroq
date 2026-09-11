# Entroq

Entroq is a general-purpose lossless compression format and codec, written in Rust.

Entroq is a new format. It is not a Rust rewrite of Zstandard, LZ4, Brotli, Snappy, or
Deflate, and it is not bitstream compatible with any of them.

## Status

Early development.

This repository holds the build entry point, the pinned toolchain, and the repository
configuration. It holds no codec, no public API, no command line tool, and no format
specification.

There is nothing to compress with yet.

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

Every action enters through the `Makefile`.

```sh
make help       # list every target
make build      # compile the workspace
make check      # type check the workspace
make validate   # format check, lint, and the dev validation tier
make ci         # run the gates on a clean Linux host, in a container
```

`make build` compiles nothing until the workspace exists.

## Documentation

`docs/` will hold the documentation. It does not exist yet.

`docs/format.md` will state the format contract. A third party must be able to write a
conforming decoder from that page alone.

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

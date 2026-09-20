# Changelog

All notable encoder generations are recorded here. The format version a stream declares is separate from the encoder version that wrote it. Every generation stays decodable by every decoder of the format version it declares.

## [0.1.0]

BALANCED baseline generation.

- New `Mode` with `Fast` and `Balanced`, defined beside the encoder engine and re-exported through the streaming pair.
- `Encoder::new` and `Encoder::with_layout` keep selecting FAST with byte-identical behavior.
- BALANCED selects only through `Encoder::balanced` and `Encoder::balanced_with_layout`.
- BALANCED matcher is the bounded hash chain at depth 32, feeding the length-lazy depth-1 parse. FAST single-hash path untouched.
- BALANCED carries the FAST search-skip schedule on incompressible input, so its RAW encode cost stays inside FAST's order of magnitude. On structured input the schedule moves the parse it selects, and its measured cost on the frozen corpus is 0 to 67 bytes per 64 KiB block.
- BALANCED declares a larger memory bound than FAST: parse steady state 524 288 bytes against 262 144, streaming steady state 3 080 308 against 2 818 164.
- No format change. No decoder change. No mode bit in the stream.

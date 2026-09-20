# Changelog

All notable encoder generations are recorded here. The format version a stream declares is separate from the encoder version that wrote it. Every generation stays decodable by every decoder of the format version it declares.

## [0.1.0]

BALANCED baseline generation.

- New `Mode` with `Fast` and `Balanced`, defined beside the encoder engine and re-exported through the streaming pair.
- `Encoder::new` and `Encoder::with_layout` keep selecting FAST with byte-identical behavior.
- BALANCED selects only through `Encoder::balanced` and `Encoder::balanced_with_layout`.
- BALANCED matcher is the bounded hash chain at depth 32. FAST single-hash path untouched.
- No format change. No decoder change. No mode bit in the stream.

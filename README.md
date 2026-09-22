# compression

An open-source, cross-platform file archiver built from scratch in Rust. The goals are to learn how compression works, and to push compression ratio by using a specialised method for each file type.

Status: **Phase 1 done**: RLE, Huffman, LZ77 and a zlib-compatible Deflate/gzip, all from scratch. **Phase 2 in progress**: adaptive range coding done, ANS next. See [docs/PLAN.md](docs/PLAN.md) for the tech stack, architecture and roadmap.

> `cmpr` is a placeholder name until the project gets a real one.

## Layout

| Path | What it is |
|---|---|
| `crates/codecs` | The algorithms. Each implements the `Codec` trait |
| `crates/testkit` | Shared round-trip test harness |
| `crates/cli` | The `cmpr` command-line tool |
| `scripts/fetch-corpora.sh` | Downloads the standard benchmark files |

## Quick start

Install Rust from <https://rustup.rs>, then:

```sh
cargo test                                   # run all tests
cargo run -p cmpr-cli -- codecs              # list codecs
cargo run -p cmpr-cli -- compress README.md  # -> README.md.cmpr
cargo run -p cmpr-cli -- decompress README.md.cmpr -o README.copy.md
cargo run -p cmpr-cli -- compress -c deflate README.md -f   # pick a codec
cargo run -p cmpr-cli -- gzip README.md                   # real .gz, opens anywhere
cargo run -p cmpr-cli -- entropy README.md                # order-0 / order-1 limits
```

## Benchmarks

```sh
scripts/fetch-corpora.sh                                   # Canterbury + Silesia
cargo run --release -p cmpr-cli -- bench corpora/silesia   # ratio + speed table
cargo bench -p cmpr-codecs                                 # detailed speed (criterion)
```

## Adding a codec

1. Create `crates/codecs/src/codecs/<name>.rs` and implement `Codec` with a new, never-reused `id`.
2. Export it from `crates/codecs/src/codecs/mod.rs` and add it to `all_codecs()` in `crates/codecs/src/lib.rs`.

That's it: the round-trip tests, fuzz-style property tests, benchmarks and CLI all pick it up automatically.

## License

[MIT](LICENSE)

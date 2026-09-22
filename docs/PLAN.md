# Archiver Project Plan

A from-scratch, open-source, cross-platform file archiver built for two goals:

1. **Learn** how compression and decompression actually work by implementing the algorithms ourselves.
2. **Compete** on compression ratio by picking the right technique for each kind of file, instead of one codec for everything.

---

## 1. Reality check: what "best compression" means

No single algorithm wins on every file. The tools that top the benchmarks win by *specialising*:

| Data type | What wins today | Why |
|---|---|---|
| Text, source code, logs | Context mixing (PAQ/cmix family), PPM | Predicts the next bit from many models at once |
| General binary | LZMA / zstd at high levels + large windows | Long-range matches |
| Executables (.exe, .dll, ELF) | BCJ/x86 filters + LZMA | Converts relative jump addresses to absolute so they repeat |
| JPEG | Lossless re-encoders (Lepton, packJPG, brunsli-style) | ~20% smaller, bit-exact restore |
| PNG, ZIP, DOCX, PDF streams | "Precomp" approach: unpack the inner Deflate, compress raw, re-pack on extract | Already-compressed data can't be squeezed again unless you undo it |
| Audio (WAV) | Linear prediction + entropy coding (FLAC/OptimFROG-style) | Samples are predictable from previous samples |
| Many similar files | Deduplication + solid archives | Store repeated chunks once |

**Our strategy:** detect what each file is, route it to a specialised pipeline (filter → model → entropy coder), and fall back to a strong general-purpose codec. That's how we get "best at all kinds of file extensions".

---

## 2. Recommended tech stack

### Core language: **Rust**

| Requirement | Why Rust fits |
|---|---|
| Speed | Compiles to native code, same league as C/C++, SIMD available |
| Safety | Archivers parse untrusted files; most real-world archiver CVEs are memory bugs. Rust prevents that class of bug by default |
| Cross-platform | One codebase builds for Windows, macOS, Linux (x86_64 + ARM64) |
| Ecosystem | Mature crates for every mainstream format, so we can compare our own codecs against reference implementations |
| Learning | Low-level control for bit-twiddling, without segfault debugging marathons |

Alternatives considered: **C++** (fastest ecosystem, but unsafe parsing and painful cross-platform builds), **Zig** (great, but young ecosystem), **Go** (GC and weaker SIMD hurt a codec engine). Rust is the best balance.

### Architecture: one engine, many front-ends

```
┌──────────────┐  ┌──────────────┐  ┌──────────────────┐
│   CLI app    │  │  Desktop GUI │  │ OS shell hooks   │
│   (clap)     │  │  (Tauri 2)   │  │ (right-click)    │
└──────┬───────┘  └──────┬───────┘  └────────┬─────────┘
       └────────────┬────┴───────────────────┘
             ┌──────▼───────┐
             │  core crate  │  archive API, job scheduler, progress, cancel
             └──────┬───────┘
     ┌──────────────┼────────────────┐
┌────▼─────┐  ┌─────▼──────┐  ┌──────▼───────┐
│ formats  │  │  codecs    │  │  analysis    │
│ zip, tar │  │ own + libs │  │ file-type    │
│ 7z, ours │  │            │  │ detection    │
└──────────┘  └────────────┘  └──────────────┘
```

### Components

| Layer | Choice | Notes |
|---|---|---|
| Workspace | Cargo workspace, multiple crates | Keeps codecs testable in isolation |
| CLI | `clap` | First front-end; fastest way to test everything |
| GUI | **Tauri 2** (Rust backend + web frontend, e.g. Svelte or SolidJS) | Small binaries (~5–10 MB), native webview, Win/macOS/Linux. Alternative: **Slint** if we want pure-Rust native UI |
| Parallelism | `rayon` + chunked/block compression | Use all cores |
| Reference codecs | `flate2` (zlib-rs backend), `zstd`, `xz2`/`liblzma`, `bzip2`, `brotli`, `lz4_flex` | Baselines to beat, and for reading/writing standard formats |
| Container formats | `zip`, `tar`, `sevenz-rust2` | Interop with the world |
| File detection | `infer` + our own magic-byte / content sniffing | Extension alone lies |
| Hashing / integrity | CRC32 (`crc32fast`), BLAKE3 | Fast checksums, dedup keys |
| Encryption | AES-256-GCM (`aes-gcm`) + Argon2id key derivation | Only when needed, via audited crates, never home-made |
| Testing | `cargo test`, `proptest` (round-trip property tests), `cargo-fuzz` | Every codec must pass `decompress(compress(x)) == x` on random input |
| Benchmarks | `criterion` + standard corpora: Canterbury, Silesia, enwik8/enwik9 | Numbers, not vibes |
| CI | GitHub Actions matrix: Windows, macOS, Linux | Build, test, fuzz smoke, benchmark |
| Packaging | `cargo-dist` (CLI), Tauri bundler (MSI, DMG, AppImage/deb/rpm) | Releases for all platforms |

### Formats to support

| Format | Read | Write | How |
|---|---|---|---|
| Our own format (working name `.cmpr` → rename later) | ✅ | ✅ | Built by us, the "best ratio" format |
| ZIP | ✅ | ✅ | Library first, own Deflate later |
| TAR, .tar.gz/.bz2/.xz/.zst | ✅ | ✅ | Libraries |
| GZ, BZ2, XZ, ZST, LZ4, Brotli | ✅ | ✅ | Libraries |
| 7z | ✅ | ✅ | Library |
| RAR | ✅ (maybe) | ❌ | Official unrar source has a non-OSI licence; needs care or a clean-room reader |

---

## 3. Our own archive format (sketch)

Designed for ratio first, but robust:

- **Header + index at the end** (like ZIP central directory) so listing is instant.
- **Per-file pipeline tag**: records which filters/codec were used, so any file can be decoded independently.
- **Solid blocks**: group similar files (by type) and compress them together for cross-file matches.
- **Content-defined chunking + dedup** (BLAKE3 hashes) for repeated data.
- **Checksums** on every block; optional recovery records later.
- **Versioned** so we can add new codecs without breaking old archives.

---

## 4. Learning roadmap (build order)

Each phase ends with something that works and a benchmark.

### Phase 0 — Foundation ✅
- Cargo workspace, CLI skeleton, CI on 3 OSes, round-trip test harness, benchmark harness with corpora.

### Phase 1 — Classic algorithms from scratch ✅
1. ✅ Bit reader/writer (LSB-first, Elias gamma codes)
2. ✅ Run-length encoding: `packbits` (byte-oriented) and `rle` (bit-level, gamma lengths)
3. ✅ Huffman coding: canonical, length-limited, table-driven decoder (`huffman` codec, `cmpr entropy`)
4. ✅ LZ77 / LZSS with hash-chain match finder and lazy matching (`lzss` codec)
5. ✅ **Deflate** (LZ77 + Huffman) + gzip: verified both ways against zlib and the real `gzip` tool (`deflate` codec, `cmpr gzip`/`gunzip`)

### Phase 2 — Modern algorithms
6. ✅ Range coder: LZMA-style binary adaptive coder, order-0 and order-1 byte models (`range0`, `range1`); `cmpr entropy` shows order-1 limits
7. ✅ rANS: static order-0, 4 interleaved states, branchless 16-bit renormalization, 32K blocks (`rans`). tANS/FSE variant later if LZ needs it
8. LZMA-style: LZ + range coder with context modelling
9. Burrows–Wheeler transform + MTF (bzip2-style)
10. Optimal parsing (smarter match selection)

### Phase 3 — Our format + smart routing
11. Container format v1 (see section 3)
12. File-type detection and codec routing
13. Solid blocks, dedup, multithreading

### Phase 4 — Specialised models (where we beat general tools)
14. Executable filters (BCJ x86/ARM64)
15. Context-mixing compressor (PAQ-inspired, logistic mixing) for text
16. Deflate recompression (precomp-style) for PNG/ZIP/DOCX/PDF
17. Lossless JPEG recompression
18. Audio/image predictors (WAV, BMP/TIFF raw data)

### Phase 5 — Product
19. Tauri GUI: browse, drag-and-drop, extract, progress, compression-level presets
20. OS integration: right-click menus, file associations
21. Encryption, split volumes, recovery records
22. Release packaging for all platforms

---

## 5. Guiding rules

- **Round-trip correctness above all**: a compressor that loses one bit is useless. Property tests + fuzzing on every codec.
- **Never trust input**: limits on memory, sizes and output length (zip bombs), path sanitising on extract (no `../` escapes).
- **Measure everything**: every codec change runs against the benchmark corpora; ratio, compress speed, decompress speed, memory.
- **Reference first, then own**: use a library to get a feature working, then replace it with our own implementation and compare.

---

## 6. Open decisions

| Decision | Recommendation | Status |
|---|---|---|
| Licence | **MIT** | ✅ Decided |
| Project / format name | TBD (placeholder: `cmpr`, `.cmpr`) | Needs your call |
| GUI toolkit | Tauri 2 (web UI) vs Slint (native Rust UI) | Decide at Phase 5, CLI first |
| Frontend framework (if Tauri) | Svelte | Decide at Phase 5 |

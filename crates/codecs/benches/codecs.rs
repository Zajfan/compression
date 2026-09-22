//! Speed benchmarks: `cargo bench -p cmpr-codecs`.
//!
//! Uses built-in synthetic inputs, plus every file under `corpora/` when it
//! exists (run `scripts/fetch-corpora.sh` first). Compression ratio is
//! reported separately by `cmpr bench`.

use cmpr_codecs::all_codecs;
use cmpr_testkit::standard_inputs;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::path::{Path, PathBuf};

/// Files larger than this are skipped to keep runs short.
const MAX_FILE: u64 = 16 << 20;

fn inputs() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = standard_inputs()
        .into_iter()
        .filter(|(_, d)| d.len() >= 1024)
        .map(|(n, d)| (n.to_string(), d))
        .collect();
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpora");
    let mut files = Vec::new();
    collect_files(&corpus, &mut files);
    files.sort();
    for path in files {
        if let Ok(data) = std::fs::read(&path) {
            let name = path
                .strip_prefix(&corpus)
                .unwrap_or(&path)
                .display()
                .to_string();
            out.push((name, data));
        }
    }
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if entry.metadata().is_ok_and(|m| m.len() <= MAX_FILE) {
            out.push(path);
        }
    }
}

fn bench(c: &mut Criterion) {
    let inputs = inputs();
    for codec in all_codecs() {
        let mut group = c.benchmark_group(format!("{}/compress", codec.name()));
        for (name, data) in &inputs {
            group.throughput(Throughput::Bytes(data.len() as u64));
            group.bench_with_input(BenchmarkId::from_parameter(name), data, |b, d| {
                b.iter(|| codec.compress(d))
            });
        }
        group.finish();

        let mut group = c.benchmark_group(format!("{}/decompress", codec.name()));
        for (name, data) in &inputs {
            let packed = codec.compress(data);
            group.throughput(Throughput::Bytes(data.len() as u64));
            group.bench_with_input(BenchmarkId::from_parameter(name), &packed, |b, p| {
                b.iter(|| codec.decompress(p, data.len()).unwrap())
            });
        }
        group.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);

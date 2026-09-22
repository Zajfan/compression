//! `cmpr bench`: compression ratio and speed per codec, with round-trip check.

use anyhow::{Context, Result, bail};
use cmpr_codecs::{Codec, all_codecs, codec_by_name};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub fn run(paths: &[PathBuf], only: &[String]) -> Result<()> {
    let codecs: Vec<Box<dyn Codec>> = if only.is_empty() {
        all_codecs()
    } else {
        only.iter()
            .map(|n| codec_by_name(n).with_context(|| format!("unknown codec '{n}'")))
            .collect::<Result<_>>()?
    };

    let mut files = Vec::new();
    for p in paths {
        collect_files(p, &mut files).with_context(|| format!("reading {}", p.display()))?;
    }
    files.sort();
    if files.is_empty() {
        bail!("no files found");
    }
    let inputs: Vec<Vec<u8>> = files
        .iter()
        .map(|f| std::fs::read(f).with_context(|| format!("reading {}", f.display())))
        .collect::<Result<_>>()?;
    let total_in: usize = inputs.iter().map(Vec::len).sum();
    println!("{} files, {} bytes\n", files.len(), total_in);
    println!(
        "{:<12} {:>14} {:>8} {:>8} {:>12} {:>12}",
        "codec", "compressed", "ratio", "bpb", "comp MB/s", "decomp MB/s"
    );

    for codec in &codecs {
        let mut total_out = 0;
        let mut comp_time = Duration::ZERO;
        let mut decomp_time = Duration::ZERO;
        for (file, data) in files.iter().zip(&inputs) {
            let t = Instant::now();
            let packed = codec.compress(data);
            comp_time += t.elapsed();
            let t = Instant::now();
            let restored = codec.decompress(&packed, data.len());
            decomp_time += t.elapsed();
            if restored.as_deref() != Ok(data.as_slice()) {
                bail!("{} failed to round-trip {}", codec.name(), file.display());
            }
            total_out += packed.len();
        }
        println!(
            "{:<12} {:>14} {:>8} {:>8.3} {:>12.1} {:>12.1}",
            codec.name(),
            total_out,
            percent(total_out, total_in),
            bits_per_byte(total_out, total_in),
            mb_per_sec(total_in, comp_time),
            mb_per_sec(total_in, decomp_time),
        );
    }
    Ok(())
}

fn collect_files(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            collect_files(&entry?.path(), out)?;
        }
    } else {
        out.push(path.to_path_buf());
    }
    Ok(())
}

/// Compressed size as a percentage of the original.
pub fn percent(compressed: usize, original: usize) -> String {
    if original == 0 {
        return "-".into();
    }
    format!("{:.2}%", compressed as f64 * 100.0 / original as f64)
}

/// Bits of output per byte of input: 8.0 means no compression.
fn bits_per_byte(compressed: usize, original: usize) -> f64 {
    if original == 0 {
        0.0
    } else {
        compressed as f64 * 8.0 / original as f64
    }
}

fn mb_per_sec(bytes: usize, time: Duration) -> f64 {
    let secs = time.as_secs_f64().max(1e-9);
    bytes as f64 / 1_000_000.0 / secs
}

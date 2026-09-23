//! `cmpr`: command-line front-end.

mod bench;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use cmpr_codecs::{all_codecs, codec_by_name, frame};
use std::path::{Path, PathBuf};

const EXTENSION: &str = "cmpr";

#[derive(Parser)]
#[command(name = "cmpr", version, about = "A from-scratch file compressor")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compress a file into a .cmpr frame
    #[command(visible_alias = "c")]
    Compress {
        input: PathBuf,
        /// Output path [default: <input>.cmpr]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Codec to use (see `cmpr codecs`)
        #[arg(short, long, default_value = "store")]
        codec: String,
        /// Overwrite the output if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Decompress a .cmpr file
    #[command(visible_alias = "d")]
    Decompress {
        input: PathBuf,
        /// Output path [default: <input> without .cmpr]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Overwrite the output if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Compress a file to standard .gz format (readable by gzip, 7-Zip, ...)
    Gzip {
        input: PathBuf,
        /// Output path [default: <input>.gz]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Compression level, 0 (store) to 9 (smallest)
        #[arg(short, long, default_value_t = 6, value_parser = clap::value_parser!(u8).range(0..=9))]
        level: u8,
        /// Overwrite the output if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Decompress a .gz file (made by any gzip tool)
    Gunzip {
        input: PathBuf,
        /// Output path [default: <input> without .gz]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Overwrite the output if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Compress a file to standard .lzma format (readable by xz, 7-Zip, ...)
    Lzma {
        input: PathBuf,
        /// Output path [default: <input>.lzma]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Overwrite the output if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Decompress a .lzma file (made by any LZMA tool)
    Unlzma {
        input: PathBuf,
        /// Output path [default: <input> without .lzma]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Overwrite the output if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Show the header of a .cmpr file
    Info { input: PathBuf },
    /// List available codecs
    Codecs,
    /// Show order-0 and order-1 entropy: the limits for byte-by-byte codecs
    /// without context (huffman, range0) and with one byte of it (range1)
    Entropy {
        /// Files or directories (searched recursively)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Measure ratio and speed of codecs on files or directories
    Bench {
        /// Files or directories (searched recursively)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Only these codecs (repeatable) [default: all]
        #[arg(short, long)]
        codec: Vec<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Compress {
            input,
            output,
            codec,
            force,
        } => {
            let codec = codec_by_name(&codec)
                .with_context(|| format!("unknown codec '{codec}' (see `cmpr codecs`)"))?;
            let output = output.unwrap_or_else(|| with_added_extension(&input, EXTENSION));
            let data = read(&input)?;
            let packed = frame::encode(codec.as_ref(), &data);
            write(&output, &packed, force)?;
            println!(
                "{} -> {}: {} -> {} bytes ({})",
                input.display(),
                output.display(),
                data.len(),
                packed.len(),
                bench::percent(packed.len(), data.len())
            );
        }
        Command::Decompress {
            input,
            output,
            force,
        } => {
            let output = match output {
                Some(o) => o,
                None if input.extension().is_some_and(|e| e == EXTENSION) => {
                    input.with_extension("")
                }
                None => bail!("input has no .{EXTENSION} extension; pass --output"),
            };
            let packed = read(&input)?;
            let data =
                frame::decode(&packed).with_context(|| format!("decoding {}", input.display()))?;
            write(&output, &data, force)?;
            println!(
                "{} -> {}: {} bytes",
                input.display(),
                output.display(),
                data.len()
            );
        }
        Command::Gzip {
            input,
            output,
            level,
            force,
        } => {
            let output = output.unwrap_or_else(|| with_added_extension(&input, "gz"));
            let data = read(&input)?;
            let packed = cmpr_codecs::gzip::compress(&data, level);
            write(&output, &packed, force)?;
            println!(
                "{} -> {}: {} -> {} bytes ({})",
                input.display(),
                output.display(),
                data.len(),
                packed.len(),
                bench::percent(packed.len(), data.len())
            );
        }
        Command::Gunzip {
            input,
            output,
            force,
        } => {
            let output = match output {
                Some(o) => o,
                None if input.extension().is_some_and(|e| e == "gz") => input.with_extension(""),
                None => bail!("input has no .gz extension; pass --output"),
            };
            let packed = read(&input)?;
            let data = cmpr_codecs::gzip::decompress(&packed, usize::MAX)
                .with_context(|| format!("decoding {}", input.display()))?;
            write(&output, &data, force)?;
            println!(
                "{} -> {}: {} bytes",
                input.display(),
                output.display(),
                data.len()
            );
        }
        Command::Lzma {
            input,
            output,
            force,
        } => {
            let output = output.unwrap_or_else(|| with_added_extension(&input, "lzma"));
            let data = read(&input)?;
            let packed = cmpr_codecs::lzma::compress(&data, Default::default());
            write(&output, &packed, force)?;
            println!(
                "{} -> {}: {} -> {} bytes ({})",
                input.display(),
                output.display(),
                data.len(),
                packed.len(),
                bench::percent(packed.len(), data.len())
            );
        }
        Command::Unlzma {
            input,
            output,
            force,
        } => {
            let output = match output {
                Some(o) => o,
                None if input.extension().is_some_and(|e| e == "lzma") => input.with_extension(""),
                None => bail!("input has no .lzma extension; pass --output"),
            };
            let packed = read(&input)?;
            let data = cmpr_codecs::lzma::decompress(&packed, usize::MAX)
                .with_context(|| format!("decoding {}", input.display()))?;
            write(&output, &data, force)?;
            println!(
                "{} -> {}: {} bytes",
                input.display(),
                output.display(),
                data.len()
            );
        }
        Command::Info { input } => {
            let packed = read(&input)?;
            let header = frame::read_header(&packed)?;
            let codec = cmpr_codecs::codec_by_id(header.codec_id).map_or_else(
                || format!("unknown ({})", header.codec_id),
                |c| c.name().to_string(),
            );
            println!("codec:           {codec}");
            println!("original size:   {} bytes", header.original_len);
            println!("compressed size: {} bytes", packed.len());
            println!("crc32:           {:08x}", header.crc32);
        }
        Command::Codecs => {
            for c in all_codecs() {
                println!("{:<3} {:<12} {}", c.id(), c.name(), c.description());
            }
        }
        Command::Entropy { paths } => bench::entropy(&paths)?,
        Command::Bench { paths, codec } => bench::run(&paths, &codec)?,
    }
    Ok(())
}

/// `file.txt` -> `file.txt.<ext>`
fn with_added_extension(path: &Path, ext: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(ext);
    name.into()
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

fn write(path: &Path, data: &[u8], force: bool) -> Result<()> {
    if !force && path.exists() {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))
}

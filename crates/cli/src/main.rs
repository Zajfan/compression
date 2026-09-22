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
    /// Show the header of a .cmpr file
    Info { input: PathBuf },
    /// List available codecs
    Codecs,
    /// Show order-0 entropy: the best any byte-by-byte codec (like huffman) can do
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
            let output = output.unwrap_or_else(|| {
                let mut name = input.clone().into_os_string();
                name.push(".");
                name.push(EXTENSION);
                name.into()
            });
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

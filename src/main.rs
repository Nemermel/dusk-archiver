#![deny(clippy::all, clippy::pedantic)]

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use dusk_archiver::{
    emit::{emit, generate_constants, write_bytes, write_text},
    ingest::{load_lua_sprites, load_png_sprites},
    metadata::Metadata,
    pack::pack,
};

/// Offline Dusk Packed Atlas (`.dpa`) compiler.
#[derive(Debug, Parser)]
#[command(name = "dusk-archiver", version, about)]
struct Cli {
    /// Legacy Lua content file providing 1-bit sprite bitmaps.
    #[arg(long, default_value = "../dusk/assets/scripts/content.lua")]
    content: PathBuf,

    /// Directory of raw `.png` sprite art.
    #[arg(long, default_value = "../dusk/assets/raw_image")]
    raw_dir: PathBuf,

    /// Optional authoring metadata (pivots, sockets, composites).
    #[arg(long, default_value = "../dusk/assets/atlas.toml")]
    metadata: PathBuf,

    /// Output `.dpa` container.
    #[arg(long, default_value = "../dusk/assets/atlas.dpa")]
    out: PathBuf,

    /// Output Rust constants module.
    #[arg(long, default_value = "../dusk/src/generated/sprites.rs")]
    constants: PathBuf,

    /// Skip generating the Rust constants module.
    #[arg(long)]
    no_constants: bool,

    /// Store the pixel payload uncompressed (development fast path).
    #[arg(long)]
    no_compress: bool,

    /// Force atlas edge length (512, 1024, or 2048).
    #[arg(long)]
    atlas_size: Option<u32>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(&cli)
}

fn run(cli: &Cli) -> Result<()> {
    let metadata = Metadata::load_or_default(&cli.metadata)?;

    let mut sources = load_lua_sprites(&cli.content)?;
    let lua_count = sources.len();
    let png_sources = load_png_sprites(&cli.raw_dir)?;
    let png_count = png_sources.len();
    sources.extend(png_sources);

    if sources.is_empty() {
        anyhow::bail!(
            "no sprites found in `{}` or `{}`",
            cli.content.display(),
            cli.raw_dir.display()
        );
    }

    let atlas = pack(sources, &metadata, cli.atlas_size)?;
    let compress = if cli.no_compress {
        false
    } else {
        metadata.compression_enabled().unwrap_or(true)
    };
    let emitted = emit(&atlas, compress)?;

    write_bytes(&cli.out, &emitted.bytes)
        .with_context(|| format!("failed to write atlas `{}`", cli.out.display()))?;

    if !cli.no_constants {
        let constants = generate_constants(&atlas)?;
        write_text(&cli.constants, &constants)
            .with_context(|| format!("failed to write constants `{}`", cli.constants.display()))?;
    }

    let raw_len = emitted.raw_payload_len();
    let stored_len = emitted.stored_payload_len();
    #[allow(clippy::cast_precision_loss)]
    let ratio = if raw_len == 0 {
        100.0
    } else {
        (stored_len as f64 / raw_len as f64) * 100.0
    };
    println!(
        "Packed {} sprites ({} Lua, {} PNG) into {}x{} atlas",
        atlas.sprites.len(),
        lua_count,
        png_count,
        atlas.width,
        atlas.height
    );
    println!(
        "  cells: {} occupied / {} available",
        atlas.occupied_cells(),
        (atlas.width / u32::from(atlas.cell_size)).pow(2)
    );
    println!(
        "  sockets: {}  composites: {} ({} parts)",
        atlas.sockets.len(),
        atlas.composites.len(),
        atlas.composite_part_count()
    );
    println!(
        "  payload: {} -> {} bytes ({:.1}%, {})",
        raw_len,
        stored_len,
        ratio,
        if compress { "lz4" } else { "none" }
    );
    println!("  wrote: {}", cli.out.display());
    if !cli.no_constants {
        println!("  constants: {}", cli.constants.display());
    }
    Ok(())
}

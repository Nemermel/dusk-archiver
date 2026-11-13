//! Source ingestion: legacy Lua sprite tables and raw PNG pixel art.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;

use anyhow::{bail, Context, Result};
use mlua::{Lua, Table, Value};

use crate::format::SpriteRenderMode;
use crate::glyphs::{frame_from_mask32, frame_from_mask8, CELL_PX};

/// One ingested sprite, already split into uniform 32x32 RGBA8 frames.
#[derive(Clone, Debug)]
pub struct SpriteSource {
    pub key: String,
    pub render_mode: SpriteRenderMode,
    pub fallback_ascii: Option<u8>,
    /// One RGBA8 frame per animation step; every frame is exactly 32x32.
    pub frames: Vec<Vec<u8>>,
    pub frame_rate_fps: u8,
    pub default_pivot: [i16; 2],
}

/// Loads every sprite from a Lua `content.lua` table.
///
/// # Errors
///
/// Returns an error when the Lua source cannot be read or evaluated, when the
/// `sprites` table is missing, or when a sprite has a malformed bitmap.
pub fn load_lua_sprites(path: &Path) -> Result<Vec<SpriteSource>> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read Lua content `{}`", path.display()))?;
    let lua = Lua::new();
    let root: Table = lua
        .load(&source)
        .set_name(path.to_string_lossy())
        .eval()
        .map_err(|error| anyhow::anyhow!("failed to evaluate `{}`: {error}", path.display()))?;
    let table: Table = root
        .get("sprites")
        .map_err(|error| anyhow::anyhow!("`{}` has no `sprites` table: {error}", path.display()))?;

    let mut sprites = Vec::new();
    for pair in table.pairs::<String, Table>() {
        let (key, tbl) =
            pair.map_err(|error| anyhow::anyhow!("failed to iterate sprite table: {error}"))?;
        sprites.push(parse_lua_sprite(key, &tbl)?);
    }
    Ok(sprites)
}

fn parse_lua_sprite(key: String, tbl: &Table) -> Result<SpriteSource> {
    let fallback_ascii = tbl
        .get::<String>("ch")
        .ok()
        .and_then(|value| value.bytes().next());
    let width = tbl.get::<u32>("width").unwrap_or(8);
    let height = tbl.get::<u32>("height").unwrap_or(8);

    let bitmap: Table = tbl
        .get("bitmap")
        .map_err(|error| anyhow::anyhow!("sprite `{key}` is missing a bitmap: {error}"))?;
    let mut rows = Vec::new();
    for value in bitmap.sequence_values::<Value>() {
        let value = value.map_err(|error| {
            anyhow::anyhow!("sprite `{key}` has an invalid bitmap row: {error}")
        })?;
        rows.push(value_to_u32(&value));
    }

    let frame = if width == 32 || height == 32 || rows.len() >= 32 {
        if rows.len() < 32 {
            bail!(
                "sprite `{key}` declares 32x32 but supplies only {} rows",
                rows.len()
            );
        }
        let mut bits = [0_u32; 32];
        for (index, value) in rows.iter().take(32).enumerate() {
            bits[index] = *value;
        }
        frame_from_mask32(&bits)
    } else {
        if rows.len() < 8 {
            bail!("sprite `{key}` supplies only {} rows", rows.len());
        }
        let mut bits = [0_u8; 8];
        for (index, value) in rows.iter().take(8).enumerate() {
            bits[index] = u8::try_from(*value).unwrap_or(0);
        }
        frame_from_mask8(&bits)
    };

    Ok(SpriteSource {
        key,
        render_mode: SpriteRenderMode::Mask,
        fallback_ascii,
        frames: vec![frame],
        frame_rate_fps: 0,
        default_pivot: [0, 0],
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn value_to_u32(value: &Value) -> u32 {
    match value {
        Value::Integer(value) => u32::try_from(*value).unwrap_or(0),
        Value::Number(value)
            if value.is_finite() && *value >= 0.0 && *value <= f64::from(u32::MAX) =>
        {
            *value as u32
        }
        _ => 0,
    }
}

/// Loads all `.png` files in `raw_dir` as RGBA sprites.
///
/// A PNG may either be a single 32x32 cell or a horizontal animation strip
/// (`N * 32` wide, `32` tall). Other dimensions are rejected because the
/// `.dpa` cell grid is uniform.
///
/// # Errors
///
/// Returns an error when the directory cannot be read, a PNG cannot be decoded,
/// or a PNG does not match the uniform cell grid.
pub fn load_png_sprites(raw_dir: &Path) -> Result<Vec<SpriteSource>> {
    if !raw_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(raw_dir)
        .with_context(|| format!("failed to read raw image dir `{}`", raw_dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read `{}`", raw_dir.display()))?;
        let path = entry.path();
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        {
            paths.push(path);
        }
    }
    paths.sort();

    let mut sprites = Vec::new();
    for path in paths {
        sprites.push(load_png(&path)?);
    }
    Ok(sprites)
}

fn load_png(path: &Path) -> Result<SpriteSource> {
    let file = File::open(path).with_context(|| format!("failed to open `{}`", path.display()))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .with_context(|| format!("failed to read PNG header `{}`", path.display()))?;

    let mut buffer = vec![0_u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .with_context(|| format!("failed to decode PNG `{}`", path.display()))?;
    let rgba = to_rgba8(&buffer[..info.buffer_size()], info.color_type, path)?;

    let width = info.width;
    let height = info.height;
    if height != CELL_PX {
        bail!(
            "`{}` is {}x{}; expected height {} for a uniform atlas cell",
            path.display(),
            width,
            height,
            CELL_PX
        );
    }
    if width == 0 || width % CELL_PX != 0 {
        bail!(
            "`{}` has width {width}; expected a multiple of {}",
            path.display(),
            CELL_PX
        );
    }

    let frame_count = width / CELL_PX;
    let mut frames = Vec::with_capacity(usize::try_from(frame_count).unwrap_or(1));
    for frame_index in 0..frame_count {
        let mut frame = vec![0_u8; (CELL_PX * CELL_PX * 4) as usize];
        let src_x = frame_index * CELL_PX;
        for row in 0_u32..CELL_PX {
            let src = ((row * width + src_x) * 4) as usize;
            let dst = (row * CELL_PX * 4) as usize;
            let len = (CELL_PX * 4) as usize;
            frame[dst..dst + len].copy_from_slice(&rgba[src..src + len]);
        }
        frames.push(frame);
    }

    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .with_context(|| format!("`{}` has a non-UTF-8 file name", path.display()))?;

    Ok(SpriteSource {
        key: key_from_file_stem(stem),
        render_mode: SpriteRenderMode::Rgba,
        fallback_ascii: None,
        frames,
        frame_rate_fps: if frame_count > 1 { 8 } else { 0 },
        default_pivot: [
            i16::try_from(CELL_PX / 2).unwrap_or(16),
            i16::try_from(CELL_PX).unwrap_or(32),
        ],
    })
}

fn to_rgba8(data: &[u8], color_type: png::ColorType, path: &Path) -> Result<Vec<u8>> {
    match color_type {
        png::ColorType::Rgba => Ok(data.to_vec()),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(data.len() / 3 * 4);
            let (chunks, _) = data.as_chunks::<3>();
            for chunk in chunks {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 0xFF]);
            }
            Ok(out)
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(data.len() * 4);
            for &gray in data {
                out.extend_from_slice(&[gray, gray, gray, 0xFF]);
            }
            Ok(out)
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(data.len() / 2 * 4);
            let (chunks, _) = data.as_chunks::<2>();
            for chunk in chunks {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            Ok(out)
        }
        png::ColorType::Indexed => {
            bail!(
                "`{}` is still palette-indexed after PNG expansion",
                path.display()
            )
        }
    }
}

/// Converts `Demon_Intruder.png` into the canonical `demon_intruder` key.
#[must_use]
pub fn key_from_file_stem(stem: &str) -> String {
    let mut key = String::new();
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            key.push(ch.to_ascii_lowercase());
        } else if !key.ends_with('_') {
            key.push('_');
        }
    }
    let trimmed = key.trim_matches('_');
    if trimmed.is_empty() {
        "unnamed".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Merges sprite sources and rejects duplicate keys.
///
/// # Errors
///
/// Returns an error when two sources normalize to the same content key.
pub fn merge_unique(mut sources: Vec<SpriteSource>) -> Result<Vec<SpriteSource>> {
    let mut seen = BTreeSet::new();
    for source in &sources {
        if !seen.insert(source.key.clone()) {
            bail!(
                "duplicate sprite key `{}` across Lua and PNG inputs",
                source.key
            );
        }
    }
    sources.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::key_from_file_stem;

    #[test]
    fn png_stems_become_snake_case_keys() {
        assert_eq!(key_from_file_stem("Demon_Intruder"), "demon_intruder");
        assert_eq!(key_from_file_stem("Void Portal"), "void_portal");
        assert_eq!(key_from_file_stem("--Weird--Name--"), "weird_name");
        assert_eq!(key_from_file_stem(""), "unnamed");
    }
}

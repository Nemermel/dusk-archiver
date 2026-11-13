//! Binary `.dpa` emission and generated Rust constant output.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use bytemuck::bytes_of;

use crate::format::{CompositeRecord, CompressionType, DpaHeader, SpriteRecord};
use crate::pack::PackedAtlas;

/// Header offset of the sprite manifest.
const HEADER_BYTES: u32 = 64;

/// Result of a successful binary emission.
#[derive(Clone, Debug)]
pub struct EmittedAtlas {
    pub bytes: Vec<u8>,
    pub header: DpaHeader,
}

impl EmittedAtlas {
    /// Raw payload length before compression.
    #[must_use]
    pub fn raw_payload_len(&self) -> usize {
        self.header.pixel_uncompressed_len as usize
    }

    /// Stored payload length after compression.
    #[must_use]
    pub fn stored_payload_len(&self) -> usize {
        self.header.pixel_payload_len as usize
    }
}

/// Serializes a packed atlas, optionally compressing the pixel payload with LZ4.
///
/// # Errors
///
/// Returns an error when a count or byte offset cannot be represented by the
/// fixed-width `.dpa` fields.
#[allow(clippy::too_many_lines)]
pub fn emit(atlas: &PackedAtlas, compress: bool) -> Result<EmittedAtlas> {
    let mut records: Vec<SpriteRecord> = atlas.sprites.iter().map(|sprite| sprite.record).collect();
    let mut string_table = Vec::new();
    for (index, sprite) in atlas.sprites.iter().enumerate() {
        let offset = u16::try_from(string_table.len())
            .context("sprite name string table exceeds u16 offset range")?;
        let name_len = u16::try_from(sprite.key.len()).context("sprite key exceeds u16 length")?;
        string_table.extend_from_slice(sprite.key.as_bytes());
        records[index].name_offset = offset;
        records[index].name_len = name_len;
    }

    let composite_part_count: usize = atlas
        .composites
        .iter()
        .map(|composite| composite.parts.len())
        .sum();

    let sprite_count = u16::try_from(records.len()).context("sprite count exceeds u16")?;
    let socket_count = u16::try_from(atlas.sockets.len()).context("socket count exceeds u16")?;
    let composite_count =
        u16::try_from(atlas.composites.len()).context("composite count exceeds u16")?;
    u16::try_from(composite_part_count).context("composite part count exceeds u16")?;

    let manifest_offset = HEADER_BYTES;
    let sockets_offset = manifest_offset
        .checked_add(
            u32::try_from(records.len())
                .unwrap_or(u32::MAX)
                .saturating_mul(32),
        )
        .context("manifest table offset overflow")?;
    let composites_offset = sockets_offset
        .checked_add(
            u32::try_from(atlas.sockets.len())
                .unwrap_or(u32::MAX)
                .saturating_mul(16),
        )
        .context("socket table offset overflow")?;
    let parts_offset = composites_offset
        .checked_add(
            u32::try_from(atlas.composites.len())
                .unwrap_or(u32::MAX)
                .saturating_mul(16),
        )
        .context("composite table offset overflow")?;
    let string_table_offset = parts_offset
        .checked_add(
            u32::try_from(composite_part_count)
                .unwrap_or(u32::MAX)
                .saturating_mul(16),
        )
        .context("composite part table offset overflow")?;
    let string_table_len = u32::try_from(string_table.len()).context("string table exceeds u32")?;
    let pixel_payload_offset = align4(
        string_table_offset
            .checked_add(string_table_len)
            .context("string table offset overflow")?,
    );

    let raw_payload = &atlas.pixels;
    let stored_payload = if compress {
        lz4_flex::block::compress(raw_payload)
    } else {
        raw_payload.clone()
    };
    let pixel_uncompressed_len =
        u32::try_from(raw_payload.len()).context("pixel payload exceeds u32")?;
    let pixel_payload_len =
        u32::try_from(stored_payload.len()).context("compressed payload exceeds u32")?;

    let mut header = DpaHeader::new(atlas.width, atlas.height, atlas.cell_size);
    header.sprite_count = sprite_count;
    header.socket_count = socket_count;
    header.composite_count = composite_count;
    header.manifest_offset = manifest_offset;
    header.sockets_offset = sockets_offset;
    header.composites_offset = composites_offset;
    header.string_table_offset = string_table_offset;
    header.string_table_len = string_table_len;
    header.pixel_payload_offset = pixel_payload_offset;
    header.pixel_payload_len = pixel_payload_len;
    header.pixel_uncompressed_len = pixel_uncompressed_len;
    header.set_compression(if compress {
        CompressionType::Lz4
    } else {
        CompressionType::None
    });

    let total_len = usize::try_from(pixel_payload_offset)
        .unwrap_or(usize::MAX)
        .saturating_add(stored_payload.len());
    let mut bytes = Vec::with_capacity(total_len);
    bytes.extend_from_slice(bytes_of(&header));
    for record in &records {
        bytes.extend_from_slice(bytes_of(record));
    }
    for socket in &atlas.sockets {
        bytes.extend_from_slice(bytes_of(socket));
    }

    let mut part_start = 0_u32;
    for composite in &atlas.composites {
        let record = CompositeRecord {
            key_hash: crate::format::hash_key(&composite.key),
            grid_w: composite.grid_w,
            grid_h: composite.grid_h,
            part_start: u16::try_from(part_start).context("composite part_start exceeds u16")?,
            part_count: u16::try_from(composite.parts.len())
                .context("composite part_count exceeds u16")?,
            reserved: [0; 2],
        };
        bytes.extend_from_slice(bytes_of(&record));
        part_start = part_start
            .checked_add(u32::try_from(composite.parts.len()).unwrap_or(u32::MAX))
            .context("composite part index overflow")?;
    }
    for composite in &atlas.composites {
        for part in &composite.parts {
            bytes.extend_from_slice(bytes_of(part));
        }
    }
    bytes.extend_from_slice(&string_table);
    while bytes.len() < usize::try_from(pixel_payload_offset).unwrap_or(usize::MAX) {
        bytes.push(0);
    }
    bytes.extend_from_slice(&stored_payload);

    Ok(EmittedAtlas { bytes, header })
}

/// Generates a Rust module with one `SPRITE_*` cell constant per sprite.
///
/// # Errors
///
/// Returns an error when two distinct sprite keys produce the same Rust
/// identifier after normalization.
pub fn generate_constants(atlas: &PackedAtlas) -> Result<String> {
    let mut names = BTreeMap::new();
    let mut lines = String::new();
    lines.push_str("// @generated by dusk-archiver. Do not edit by hand.\n");
    writeln!(
        lines,
        "// Atlas: {}x{}, cell: {}, sprites: {}\n",
        atlas.width,
        atlas.height,
        atlas.cell_size,
        atlas.sprites.len()
    )
    .expect("writing to a String cannot fail");
    writeln!(
        lines,
        "pub const SPRITE_ATLAS_WIDTH: u32 = {};",
        atlas.width
    )
    .expect("writing to a String cannot fail");
    writeln!(
        lines,
        "pub const SPRITE_ATLAS_HEIGHT: u32 = {};",
        atlas.height
    )
    .expect("writing to a String cannot fail");
    writeln!(
        lines,
        "pub const SPRITE_CELL_SIZE: u16 = {};",
        atlas.cell_size
    )
    .expect("writing to a String cannot fail");
    writeln!(
        lines,
        "pub const SPRITE_COUNT: u16 = {};\n",
        atlas.sprites.len()
    )
    .expect("writing to a String cannot fail");

    for sprite in &atlas.sprites {
        let ident = constant_name(&sprite.key);
        if let Some(previous) = names.insert(ident.clone(), sprite.key.clone()) {
            bail!(
                "constants `{ident}` collide for sprite keys `{previous}` and `{}`",
                sprite.key
            );
        }
        writeln!(
            lines,
            "pub const {ident}: u16 = {};",
            sprite.record.cell_start
        )
        .expect("writing to a String cannot fail");
    }
    Ok(lines)
}

/// Writes bytes to `path`, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error when the parent directory or file cannot be written.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Writes a text file, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error when the parent directory or file cannot be written.
pub fn write_text(path: &Path, text: &str) -> Result<()> {
    write_bytes(path, text.as_bytes())
}

fn constant_name(key: &str) -> String {
    let mut ident = String::from("SPRITE_");
    let mut last_underscore = true;
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() {
            ident.push(ch.to_ascii_uppercase());
            last_underscore = false;
        } else if !last_underscore {
            ident.push('_');
            last_underscore = true;
        }
    }
    while ident.ends_with('_') {
        ident.pop();
    }
    ident
}

const fn align4(value: u32) -> u32 {
    let remainder = value % 4;
    if remainder == 0 {
        value
    } else {
        value + (4 - remainder)
    }
}

#[cfg(test)]
mod tests {
    use crate::format::{CompositePartRecord, DpaHeader, SpriteRenderMode, FLAG_COMPRESSED};
    use crate::glyphs::FRAME_BYTES;
    use crate::ingest::SpriteSource;
    use crate::metadata::{CompositeMeta, CompositePartMeta, Metadata};
    use crate::pack::{pack, PackedAtlas};

    use super::{emit, generate_constants};

    fn atlas() -> PackedAtlas {
        let source = SpriteSource {
            key: "demon_intruder".to_owned(),
            render_mode: SpriteRenderMode::Rgba,
            fallback_ascii: Some(b'd'),
            frames: vec![vec![0x42; FRAME_BYTES]],
            frame_rate_fps: 8,
            default_pivot: [16, 32],
        };
        let mut metadata = Metadata::default();
        metadata.composite.insert(
            "portal".to_owned(),
            CompositeMeta {
                grid_size: [3, 3],
                parts: vec![CompositePartMeta {
                    sprite: "demon_intruder".to_owned(),
                    grid_pos: [1, 2],
                    pixel_offset: [0, -4],
                    z: 31,
                }],
                ..CompositeMeta::default()
            },
        );
        pack(vec![source], &metadata, Some(512)).unwrap()
    }

    #[test]
    fn header_offsets_are_consistent_and_compress_roundtrips() {
        let atlas = atlas();
        let emitted = emit(&atlas, true).unwrap();
        assert_eq!(
            emitted.stored_payload_len(),
            emitted.bytes.len() - emitted.header.pixel_payload_offset as usize
        );
        assert!(emitted.stored_payload_len() < emitted.raw_payload_len());
        assert_eq!(emitted.header.flags & FLAG_COMPRESSED, FLAG_COMPRESSED);

        let header: &DpaHeader =
            bytemuck::from_bytes(&emitted.bytes[..std::mem::size_of::<DpaHeader>()]);
        assert_eq!(header.magic, *b"DPAK");
        assert_eq!(header.sprite_count, 1);
        assert_eq!(header.composite_count, 1);
        assert_eq!(header.manifest_offset, 64);
        assert_eq!(header.sockets_offset, 64 + 32);
        assert_eq!(header.composites_offset, 64 + 32);
        // One composite header (16) plus one composite part (16).
        assert_eq!(header.string_table_offset, 64 + 32 + 16 + 16);
        assert_eq!(header.pixel_payload_offset % 4, 0);

        let raw = lz4_flex::block::decompress(
            &emitted.bytes[header.pixel_payload_offset as usize
                ..header.pixel_payload_offset as usize + header.pixel_payload_len as usize],
            header.pixel_uncompressed_len as usize,
        )
        .unwrap();
        assert_eq!(raw, atlas.pixels);
    }

    #[test]
    fn composite_parts_are_written_after_composite_headers() {
        let emitted = emit(&atlas(), false).unwrap();
        let header = emitted.header;
        let part_offset = header.composites_offset as usize + 16;
        let part: &CompositePartRecord = bytemuck::from_bytes(
            &emitted.bytes[part_offset..part_offset + std::mem::size_of::<CompositePartRecord>()],
        );
        assert_eq!(part.grid_offset_y, 2);
        assert_eq!(part.local_z, 31);
    }

    #[test]
    fn constants_are_sorted_and_collision_checked() {
        let text = generate_constants(&atlas()).unwrap();
        assert!(text.contains("pub const SPRITE_DEMON_INTRUDER: u16 = 128;"));
        assert!(text.contains("pub const SPRITE_COUNT: u16 = 1;"));
    }
}

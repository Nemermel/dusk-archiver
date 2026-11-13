//! Cell layout, metadata application, socket/composite compilation, and atlas
//! rasterization.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};

use crate::format::{
    hash_key, hash_name, CompositePartRecord, SocketRecord, SpriteRecord, SpriteRenderMode,
    SOCKET_FLAG_MIRROR_WITH_PARENT,
};
use crate::glyphs::{ascii_frame, stamp_frame, CELL_PX, FALLBACK_SOLID_CELL, SPRITE_BASE};
use crate::ingest::{merge_unique, SpriteSource};
use crate::metadata::{Metadata, SpriteMeta};

/// Supported atlas edge lengths.
pub const ATLAS_SIZES: [u32; 3] = [512, 1024, 2048];
/// Maximum addressable cells in a 2048x2048 atlas.
pub const MAX_CELLS: u32 = 4096;

/// A sprite after metadata application and cell allocation.
#[derive(Clone, Debug)]
pub struct PackedSprite {
    pub key: String,
    pub record: SpriteRecord,
    pub frames: Vec<Vec<u8>>,
}

/// A compiled composite assembly ready for binary emission.
#[derive(Clone, Debug)]
pub struct PackedComposite {
    pub key: String,
    pub grid_w: u8,
    pub grid_h: u8,
    pub parts: Vec<CompositePartRecord>,
}

/// Fully rasterized atlas plus its manifest tables.
#[derive(Clone, Debug)]
pub struct PackedAtlas {
    pub width: u32,
    pub height: u32,
    pub cell_size: u16,
    pub pixels: Vec<u8>,
    pub sprites: Vec<PackedSprite>,
    pub sockets: Vec<SocketRecord>,
    pub composites: Vec<PackedComposite>,
}

impl PackedAtlas {
    /// Number of occupied cells (including the reserved ASCII block).
    #[must_use]
    pub fn occupied_cells(&self) -> u32 {
        self.sprites
            .iter()
            .map(|sprite| u32::from(sprite.record.frame_count))
            .sum::<u32>()
            + u32::from(SPRITE_BASE)
    }

    /// Number of composite parts across all composites.
    #[must_use]
    pub fn composite_part_count(&self) -> usize {
        self.composites
            .iter()
            .map(|composite| composite.parts.len())
            .sum()
    }
}

/// Applies metadata, allocates cells, rasterizes pixels, and builds tables.
///
/// # Errors
///
/// Returns an error when metadata references unknown sprites, frame counts
/// disagree with the inputs, the content exceeds the atlas cell capacity, or a
/// table index cannot be represented by the on-disk record fields.
#[allow(clippy::too_many_lines)]
pub fn pack(
    sources: Vec<SpriteSource>,
    metadata: &Metadata,
    forced_atlas_size: Option<u32>,
) -> Result<PackedAtlas> {
    let sources = merge_unique(sources)?;
    let keys: BTreeSet<String> = sources.iter().map(|source| source.key.clone()).collect();
    metadata.validate(&keys)?;

    let mut sprites: Vec<PackedSprite> = Vec::with_capacity(sources.len());
    let mut socket_table = Vec::new();
    let mut next_cell = u32::from(SPRITE_BASE);

    for source in sources {
        let meta = metadata.sprite.get(&source.key);
        let render_mode = parse_render_mode(meta).unwrap_or(source.render_mode);
        let frame_count = u8::try_from(source.frames.len()).with_context(|| {
            format!(
                "sprite `{}` has {} frames; the format supports at most 255",
                source.key,
                source.frames.len()
            )
        })?;
        if frame_count == 0 {
            bail!("sprite `{}` has no frames", source.key);
        }
        if let Some(meta) = meta {
            if let Some(expected) = meta.frame_count {
                if expected != frame_count {
                    bail!(
                        "sprite `{}` metadata expects {} frames but the input has {}",
                        source.key,
                        expected,
                        frame_count
                    );
                }
            }
        }
        if next_cell >= MAX_CELLS {
            bail!(
                "atlas exceeds {} cells at sprite `{}`; reduce sprite/frame count",
                MAX_CELLS,
                source.key
            );
        }
        let cell_start = u16::try_from(next_cell)
            .with_context(|| format!("cell index for sprite `{}` exceeds u16", source.key))?;
        next_cell = next_cell
            .checked_add(u32::from(frame_count))
            .context("atlas cell allocation overflow")?;
        if next_cell > MAX_CELLS {
            bail!(
                "sprite `{}` ({} frames) does not fit in the {} cell limit",
                source.key,
                frame_count,
                MAX_CELLS
            );
        }

        let frame_rate_fps = meta
            .and_then(|meta| meta.frame_rate_fps)
            .unwrap_or(source.frame_rate_fps);
        let fallback_ascii = parse_fallback_ascii(meta)?
            .or(source.fallback_ascii)
            .unwrap_or(0);
        let pivot = meta
            .and_then(|meta| meta.pivot)
            .unwrap_or(source.default_pivot);
        let mut sprite_sockets = build_sockets(&source.key, metadata, &keys)?;
        let socket_count =
            u16::try_from(sprite_sockets.len()).context("per-sprite socket count exceeds u16")?;
        let socket_start_idx = if sprite_sockets.is_empty() {
            0
        } else {
            u16::try_from(socket_table.len()).context("socket table index exceeds u16")?
        };
        socket_table.append(&mut sprite_sockets);

        let key_hash = hash_key(&source.key);
        sprites.push(PackedSprite {
            key: source.key,
            record: SpriteRecord {
                key_hash,
                cell_start,
                frame_count,
                frame_rate_fps,
                render_mode: render_mode as u8,
                fallback_ascii,
                pivot_x: pivot[0],
                pivot_y: pivot[1],
                width_px: u16::try_from(CELL_PX).unwrap_or(32),
                height_px: u16::try_from(CELL_PX).unwrap_or(32),
                socket_start_idx,
                socket_count,
                name_offset: 0,
                name_len: 0,
                reserved: 0,
            },
            frames: source.frames,
        });
    }

    let atlas_size = choose_atlas_size(metadata, forced_atlas_size, next_cell)?;
    let atlas_cols = atlas_size / CELL_PX;
    let mut pixels = vec![0_u8; (atlas_size * atlas_size * 4) as usize];

    // Reserved ASCII/text block: cells 0..=127.
    for code in 0_u16..SPRITE_BASE {
        let code_u8 = u8::try_from(code).unwrap_or(0);
        let frame = ascii_frame(code_u8);
        stamp_frame(&mut pixels, atlas_size, atlas_cols, code, &frame);
    }
    debug_assert_eq!(FALLBACK_SOLID_CELL, SPRITE_BASE - 1);

    for sprite in &sprites {
        for (frame_index, frame) in sprite.frames.iter().enumerate() {
            let offset = u16::try_from(frame_index).context("frame index exceeds u16")?;
            let cell = sprite
                .record
                .cell_start
                .checked_add(offset)
                .context("sprite cell range overflow")?;
            stamp_frame(&mut pixels, atlas_size, atlas_cols, cell, frame);
        }
    }

    let composites = build_composites(metadata)?;

    Ok(PackedAtlas {
        width: atlas_size,
        height: atlas_size,
        cell_size: u16::try_from(CELL_PX).unwrap_or(32),
        pixels,
        sprites,
        sockets: socket_table,
        composites,
    })
}

fn choose_atlas_size(metadata: &Metadata, forced: Option<u32>, occupied_cells: u32) -> Result<u32> {
    let requested = forced.or(metadata.atlas.size);
    if let Some(size) = requested {
        if !ATLAS_SIZES.contains(&size) {
            bail!("atlas size {size} is unsupported; expected 512, 1024, or 2048");
        }
        let capacity = (size / CELL_PX) * (size / CELL_PX);
        if occupied_cells > capacity {
            bail!("atlas {size}x{size} holds {capacity} cells but content needs {occupied_cells}");
        }
        return Ok(size);
    }

    ATLAS_SIZES
        .into_iter()
        .find(|&size| {
            let capacity = (size / CELL_PX) * (size / CELL_PX);
            occupied_cells <= capacity
        })
        .with_context(|| {
            format!("content needs {occupied_cells} cells, exceeding the 2048x2048 atlas")
        })
}

fn parse_render_mode(meta: Option<&SpriteMeta>) -> Option<SpriteRenderMode> {
    meta.and_then(|meta| meta.render_mode.as_deref())
        .map(|mode| match mode {
            "rgba" | "RGBA" | "Rgba" => SpriteRenderMode::Rgba,
            _ => SpriteRenderMode::Mask,
        })
}

fn parse_fallback_ascii(meta: Option<&SpriteMeta>) -> Result<Option<u8>> {
    let Some(value) = meta.and_then(|meta| meta.ascii.as_deref()) else {
        return Ok(None);
    };
    let mut bytes = value.bytes();
    let first = bytes
        .next()
        .with_context(|| "sprite metadata `ascii` cannot be empty")?;
    if bytes.next().is_some() {
        bail!("sprite metadata `ascii` must be exactly one ASCII byte");
    }
    Ok(Some(first))
}

fn build_sockets(
    sprite_key: &str,
    metadata: &Metadata,
    keys: &BTreeSet<String>,
) -> Result<Vec<SocketRecord>> {
    let Some(sockets) = metadata.socket.get(sprite_key) else {
        return Ok(Vec::new());
    };
    let mut records = Vec::with_capacity(sockets.len());
    for (name, socket) in sockets {
        // `BTreeMap` already yields deterministic name order.
        if !keys.contains(sprite_key) {
            bail!("socket metadata references unknown parent sprite `{sprite_key}`");
        }
        records.push(SocketRecord {
            name_hash: hash_name(name),
            offset_x: socket.offset[0],
            offset_y: socket.offset[1],
            z_offset: socket.z,
            flags: if socket.mirror {
                SOCKET_FLAG_MIRROR_WITH_PARENT
            } else {
                0
            },
            reserved: [0; 6],
        });
    }
    Ok(records)
}

fn build_composites(metadata: &Metadata) -> Result<Vec<PackedComposite>> {
    let mut composites = Vec::with_capacity(metadata.composite.len());
    for (key, composite) in &metadata.composite {
        let mut parts = Vec::with_capacity(composite.parts.len());
        for part in &composite.parts {
            parts.push(CompositePartRecord {
                sprite_key_hash: hash_key(&part.sprite),
                grid_offset_x: part.grid_pos[0],
                grid_offset_y: part.grid_pos[1],
                pixel_offset_x: part.pixel_offset[0],
                pixel_offset_y: part.pixel_offset[1],
                local_z: part.z,
                reserved: 0,
            });
        }
        u16::try_from(parts.len()).context("composite part count exceeds u16")?;
        composites.push(PackedComposite {
            key: key.clone(),
            grid_w: composite.grid_size[0],
            grid_h: composite.grid_size[1],
            parts,
        });
    }
    Ok(composites)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{choose_atlas_size, pack};
    use crate::format::SpriteRenderMode;
    use crate::glyphs::FRAME_BYTES;
    use crate::ingest::SpriteSource;
    use crate::metadata::{CompositeMeta, CompositePartMeta, Metadata, SocketMeta};

    fn source(key: &str, frames: usize) -> SpriteSource {
        SpriteSource {
            key: key.to_owned(),
            render_mode: SpriteRenderMode::Rgba,
            fallback_ascii: None,
            frames: vec![vec![0xFF; FRAME_BYTES]; frames],
            frame_rate_fps: 8,
            default_pivot: [16, 32],
        }
    }

    #[test]
    fn selects_smallest_fitting_atlas() {
        let meta = Metadata::default();
        assert_eq!(choose_atlas_size(&meta, None, 128).unwrap(), 512);
        assert_eq!(choose_atlas_size(&meta, None, 257).unwrap(), 1024);
        assert_eq!(choose_atlas_size(&meta, None, 1025).unwrap(), 2048);
        assert!(choose_atlas_size(&meta, None, 4097).is_err());
        assert!(choose_atlas_size(&meta, Some(512), 300).is_err());
    }

    #[test]
    fn packs_cells_frames_sockets_and_composites() {
        let mut metadata = Metadata::default();
        metadata.sprite.insert(
            "demon_intruder".to_owned(),
            crate::metadata::SpriteMeta {
                frame_rate_fps: Some(12),
                ..crate::metadata::SpriteMeta::default()
            },
        );
        metadata.socket.insert(
            "demon_intruder".to_owned(),
            [(
                "hand_r".to_owned(),
                SocketMeta {
                    offset: [12, -16],
                    z: 1,
                    mirror: true,
                },
            )]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        );
        metadata.composite.insert(
            "portal".to_owned(),
            CompositeMeta {
                grid_size: [2, 2],
                parts: vec![CompositePartMeta {
                    sprite: "demon_intruder".to_owned(),
                    grid_pos: [0, 0],
                    pixel_offset: [0, 0],
                    z: 3,
                }],
                ..CompositeMeta::default()
            },
        );

        let atlas = pack(
            vec![source("demon_intruder", 2), source("floor_ash", 1)],
            &metadata,
            Some(512),
        )
        .unwrap();

        assert_eq!(atlas.width, 512);
        assert_eq!(atlas.sprites.len(), 2);
        assert_eq!(atlas.sprites[0].key, "demon_intruder");
        assert_eq!(atlas.sprites[0].record.cell_start, 128);
        assert_eq!(atlas.sprites[0].record.frame_count, 2);
        assert_eq!(atlas.sprites[0].record.frame_rate_fps, 12);
        assert_eq!(atlas.sprites[0].record.socket_count, 1);
        assert_eq!(atlas.sprites[1].record.cell_start, 130);
        assert_eq!(atlas.sprites[1].record.socket_start_idx, 0);
        assert_eq!(atlas.sprites[1].record.socket_count, 0);
        assert_eq!(atlas.sockets.len(), 1);
        assert_eq!(atlas.composites.len(), 1);
        assert_eq!(atlas.composite_part_count(), 1);
        assert_eq!(atlas.occupied_cells(), 131);
    }

    #[test]
    fn rejects_frame_count_mismatch() {
        let mut metadata = Metadata::default();
        metadata.sprite.insert(
            "demon_intruder".to_owned(),
            crate::metadata::SpriteMeta {
                frame_count: Some(4),
                ..crate::metadata::SpriteMeta::default()
            },
        );
        assert!(pack(vec![source("demon_intruder", 2)], &metadata, Some(512)).is_err());
    }

    #[test]
    fn rejects_more_than_255_frames() {
        let mut sprite = source("demon_intruder", 1);
        sprite.frames = vec![vec![0_u8; FRAME_BYTES]; 256];
        assert!(pack(vec![sprite], &Metadata::default(), Some(2048)).is_err());
    }
}

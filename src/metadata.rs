//! Optional authoring metadata for sprites, sockets, and composite assemblies.
//!
//! The schema is intentionally close to the TOML examples in the architecture
//! spec. Every table is optional; `dusk-archiver` can pack a bare `content.lua`
//! plus `raw_image/` directory with sensible defaults.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Root metadata document (`assets/atlas.toml`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub atlas: AtlasMeta,
    /// Per-sprite overrides keyed by content key.
    pub sprite: std::collections::BTreeMap<String, SpriteMeta>,
    /// Socket definitions: parent sprite key -> socket name -> socket.
    pub socket: std::collections::BTreeMap<String, std::collections::BTreeMap<String, SocketMeta>>,
    /// Composite assembly definitions keyed by composite key.
    pub composite: std::collections::BTreeMap<String, CompositeMeta>,
}

/// Global atlas settings.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AtlasMeta {
    /// Preferred atlas edge length (512, 1024, or 2048). Auto-grown when absent.
    pub size: Option<u32>,
    /// `"lz4"` or `"none"`.
    pub compression: Option<String>,
}

/// Per-sprite overrides.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SpriteMeta {
    /// ASCII fallback glyph, as a one-character string.
    pub ascii: Option<String>,
    /// Pivot in pixels from the sprite's top-left corner.
    pub pivot: Option<[i16; 2]>,
    /// Expected frame count for animation strips.
    pub frame_count: Option<u8>,
    /// Playback rate in frames per second.
    pub frame_rate_fps: Option<u8>,
    /// `"mask"` or `"rgba"`.
    pub render_mode: Option<String>,
}

/// One socket/attachment point.
#[derive(Clone, Debug, Deserialize)]
pub struct SocketMeta {
    /// Offset relative to the parent sprite's pivot.
    pub offset: [i16; 2],
    /// Layer bias relative to the parent sprite.
    #[serde(default)]
    pub z: i8,
    /// Mirror the X offset when the parent sprite is flipped.
    #[serde(default = "default_true")]
    pub mirror: bool,
}

/// One composite / multi-tile assembly.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CompositeMeta {
    /// Occupied grid size `[width, height]`.
    pub grid_size: [u8; 2],
    /// Interaction anchor in local grid coordinates.
    pub interaction_tile: Option<[u8; 2]>,
    /// Collision footprint mask (`1` = blocking), validated but not serialized
    /// into the `.dpa` composite table.
    pub footprint: Option<Vec<Vec<u8>>>,
    /// Layered sprite parts.
    pub parts: Vec<CompositePartMeta>,
}

/// One sprite layer inside a composite.
#[derive(Clone, Debug, Deserialize)]
pub struct CompositePartMeta {
    /// Sprite content key.
    pub sprite: String,
    /// Grid offset from the composite's top-left cell.
    #[serde(default)]
    pub grid_pos: [i8; 2],
    /// Pixel offset inside the target cell.
    #[serde(default)]
    pub pixel_offset: [i16; 2],
    /// Local Z bias; `z_layer` is accepted as an alias.
    #[serde(default, alias = "z_layer")]
    pub z: i8,
}

impl Metadata {
    /// Loads metadata from `path`. A missing file is treated as an empty document.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or parsed.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read metadata `{}`", path.display()))?;
        toml::from_str(&source)
            .with_context(|| format!("failed to parse metadata `{}`", path.display()))
    }

    /// Returns the configured compression mode, if any.
    #[must_use]
    pub fn compression_enabled(&self) -> Option<bool> {
        self.atlas
            .compression
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("lz4"))
    }

    /// Validates metadata against the sprite keys discovered from Lua + PNG.
    ///
    /// # Errors
    ///
    /// Returns an error when a socket or composite references an unknown sprite,
    /// or when a composite grid, footprint, or part placement is inconsistent.
    pub fn validate(&self, sprite_keys: &BTreeSet<String>) -> Result<()> {
        for (parent, sockets) in &self.socket {
            if !sprite_keys.contains(parent) {
                bail!("socket metadata references unknown parent sprite `{parent}`");
            }
            for name in sockets.keys() {
                if name.is_empty() {
                    bail!("socket on `{parent}` has an empty name");
                }
            }
        }

        for (key, composite) in &self.composite {
            if composite.grid_size[0] == 0 || composite.grid_size[1] == 0 {
                bail!("composite `{key}` has a zero-sized grid");
            }
            if let Some(footprint) = &composite.footprint {
                let expected = usize::from(composite.grid_size[1]);
                if footprint.len() != expected {
                    bail!(
                        "composite `{key}` footprint has {} rows, expected {}",
                        footprint.len(),
                        expected
                    );
                }
                for (row_idx, row) in footprint.iter().enumerate() {
                    if row.len() != usize::from(composite.grid_size[0]) {
                        bail!(
                            "composite `{key}` footprint row {row_idx} has {} columns, expected {}",
                            row.len(),
                            composite.grid_size[0]
                        );
                    }
                }
            }
            if let Some([tx, ty]) = composite.interaction_tile {
                if tx >= composite.grid_size[0] || ty >= composite.grid_size[1] {
                    bail!("composite `{key}` interaction tile [{tx}, {ty}] lies outside its grid");
                }
            }
            if composite.parts.is_empty() {
                bail!("composite `{key}` has no parts");
            }
            for part in &composite.parts {
                if !sprite_keys.contains(&part.sprite) {
                    bail!(
                        "composite `{key}` references unknown sprite `{}`",
                        part.sprite
                    );
                }
                if part.grid_pos[0] < 0
                    || part.grid_pos[1] < 0
                    || u8::try_from(part.grid_pos[0]).unwrap_or(u8::MAX) >= composite.grid_size[0]
                    || u8::try_from(part.grid_pos[1]).unwrap_or(u8::MAX) >= composite.grid_size[1]
                {
                    bail!(
                        "composite `{key}` part `{}` lies outside the grid",
                        part.sprite
                    );
                }
            }
        }
        Ok(())
    }
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::Metadata;
    use std::collections::BTreeSet;

    fn keys(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_sprite_socket_and_composite_tables() {
        let source = r#"
[atlas]
size = 512
compression = "lz4"

[sprite.demon_intruder]
ascii = "d"
pivot = [16, 32]
frame_rate_fps = 8

[socket.demon_intruder.hand_r]
offset = [12, -16]
z = 1

[composite.breach_portal_colossus]
grid_size = [3, 3]
interaction_tile = [1, 2]
footprint = [[0, 1, 0], [1, 1, 1], [0, 1, 0]]
parts = [
    { sprite = "portal_core_swirl", grid_pos = [0, 0], pixel_offset = [0, 0], z_layer = 31 },
    { sprite = "portal_arch_frame", grid_pos = [0, 0], pixel_offset = [0, 0], z_layer = 35 },
]
"#;
        let meta: Metadata = toml::from_str(source).unwrap();
        assert_eq!(meta.atlas.size, Some(512));
        assert_eq!(meta.compression_enabled(), Some(true));
        assert_eq!(meta.sprite["demon_intruder"].pivot, Some([16, 32]));
        assert_eq!(meta.socket["demon_intruder"]["hand_r"].offset, [12, -16]);
        assert!(meta.socket["demon_intruder"]["hand_r"].mirror);
        assert_eq!(meta.composite["breach_portal_colossus"].parts.len(), 2);
        assert_eq!(meta.composite["breach_portal_colossus"].parts[1].z, 35);
    }

    #[test]
    fn validates_socket_parents_and_composite_parts() {
        let meta = Metadata {
            socket: [(
                "missing".to_owned(),
                [(
                    "hand".to_owned(),
                    super::SocketMeta {
                        offset: [1, 2],
                        z: 0,
                        mirror: true,
                    },
                )]
                .into_iter()
                .collect(),
            )]
            .into_iter()
            .collect(),
            ..Metadata::default()
        };
        assert!(meta.validate(&keys(&["other"])).is_err());

        let bad_composite = Metadata {
            composite: [(
                "bad".to_owned(),
                super::CompositeMeta {
                    grid_size: [2, 2],
                    parts: vec![super::CompositePartMeta {
                        sprite: "missing".to_owned(),
                        grid_pos: [0, 0],
                        pixel_offset: [0, 0],
                        z: 0,
                    }],
                    ..super::CompositeMeta::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Metadata::default()
        };
        assert!(bad_composite.validate(&keys(&["other"])).is_err());
    }
}

//! On-disk `.dpa` format primitives.
//!
//! These layouts mirror `dusk::dpa` (`src/dpa.rs` in the engine repository) and
//! the specification in
//! `DuskDocs/40_Engineering/41_Architecture/Sprite_Atlas_Architecture.md`.
//! The archiver is intentionally standalone: it does not link `wgpu`, `winit`,
//! or the runtime ECS just to write a file.

use bytemuck::{Pod, Zeroable};

/// Container magic: `b"DPAK"`.
pub const MAGIC: [u8; 4] = *b"DPAK";
/// Current on-disk format version.
pub const FORMAT_VERSION: u16 = 1;

/// Header flag: pixel payload is compressed.
pub const FLAG_COMPRESSED: u16 = 1 << 0;
/// Header flag: pixel payload uses premultiplied alpha.
pub const FLAG_PREMULTIPLIED_ALPHA: u16 = 1 << 1;

/// Socket flag: mirror this socket's X offset when the parent sprite is flipped.
pub const SOCKET_FLAG_MIRROR_WITH_PARENT: u8 = 1 << 0;

const FNV1A_64_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01B3;
const FNV1A_32_OFFSET: u32 = 0x811C_9DC5;
const FNV1A_32_PRIME: u32 = 0x0100_0193;

/// Stable FNV-1a 64-bit hash for content keys.
#[must_use]
pub fn hash_key(key: &str) -> u64 {
    let mut hash = FNV1A_64_OFFSET;
    for &byte in key.as_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
    }
    hash
}

/// Stable FNV-1a 32-bit hash for socket names.
#[must_use]
pub fn hash_name(name: &str) -> u32 {
    let mut hash = FNV1A_32_OFFSET;
    for &byte in name.as_bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(FNV1A_32_PRIME);
    }
    hash
}

/// Pixel decoding mode for a sprite.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpriteRenderMode {
    /// 1-bit monochrome mask.
    #[default]
    Mask = 0,
    /// Full-color RGBA pixel art.
    Rgba = 1,
}

/// Pixel compression scheme.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompressionType {
    /// Raw, GPU-ready bytes.
    #[default]
    None = 0,
    /// A single LZ4 block.
    Lz4 = 1,
}

/// Fixed 64-byte `.dpa` header.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct DpaHeader {
    pub magic: [u8; 4],
    pub version: u16,
    pub flags: u16,
    pub compression_type: u8,
    pub reserved0: [u8; 7],
    pub atlas_width: u32,
    pub atlas_height: u32,
    pub cell_size: u16,
    pub sprite_count: u16,
    pub socket_count: u16,
    pub composite_count: u16,
    pub manifest_offset: u32,
    pub sockets_offset: u32,
    pub composites_offset: u32,
    pub string_table_offset: u32,
    pub string_table_len: u32,
    pub pixel_payload_offset: u32,
    pub pixel_payload_len: u32,
    pub pixel_uncompressed_len: u32,
}

impl DpaHeader {
    /// Builds a zeroed, valid version-1 header.
    #[must_use]
    pub const fn new(atlas_width: u32, atlas_height: u32, cell_size: u16) -> Self {
        Self {
            magic: MAGIC,
            version: FORMAT_VERSION,
            flags: 0,
            compression_type: CompressionType::None as u8,
            reserved0: [0; 7],
            atlas_width,
            atlas_height,
            cell_size,
            sprite_count: 0,
            socket_count: 0,
            composite_count: 0,
            manifest_offset: 0,
            sockets_offset: 0,
            composites_offset: 0,
            string_table_offset: 0,
            string_table_len: 0,
            pixel_payload_offset: 0,
            pixel_payload_len: 0,
            pixel_uncompressed_len: 0,
        }
    }

    /// Applies the compression mode and keeps [`FLAG_COMPRESSED`] in sync.
    pub const fn set_compression(&mut self, compression: CompressionType) {
        self.compression_type = compression as u8;
        if matches!(compression, CompressionType::Lz4) {
            self.flags |= FLAG_COMPRESSED;
        } else {
            self.flags &= !FLAG_COMPRESSED;
        }
    }
}

/// Fixed 32-byte sprite manifest entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct SpriteRecord {
    pub key_hash: u64,
    pub cell_start: u16,
    pub frame_count: u8,
    pub frame_rate_fps: u8,
    pub render_mode: u8,
    pub fallback_ascii: u8,
    pub pivot_x: i16,
    pub pivot_y: i16,
    pub width_px: u16,
    pub height_px: u16,
    pub socket_start_idx: u16,
    pub socket_count: u16,
    pub name_offset: u16,
    pub name_len: u16,
    pub reserved: u16,
}

/// Fixed 16-byte socket / attachment-point entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct SocketRecord {
    pub name_hash: u32,
    pub offset_x: i16,
    pub offset_y: i16,
    pub z_offset: i8,
    pub flags: u8,
    pub reserved: [u8; 6],
}

/// Fixed 16-byte composite header.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct CompositeRecord {
    pub key_hash: u64,
    pub grid_w: u8,
    pub grid_h: u8,
    pub part_start: u16,
    pub part_count: u16,
    pub reserved: [u8; 2],
}

/// Fixed 16-byte composite part entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct CompositePartRecord {
    pub sprite_key_hash: u64,
    pub grid_offset_x: i8,
    pub grid_offset_y: i8,
    pub pixel_offset_x: i16,
    pub pixel_offset_y: i16,
    pub local_z: i8,
    pub reserved: u8,
}

const _: () = assert!(std::mem::size_of::<DpaHeader>() == 64);
const _: () = assert!(std::mem::size_of::<SpriteRecord>() == 32);
const _: () = assert!(std::mem::size_of::<SocketRecord>() == 16);
const _: () = assert!(std::mem::size_of::<CompositeRecord>() == 16);
const _: () = assert!(std::mem::size_of::<CompositePartRecord>() == 16);

#[cfg(test)]
mod tests {
    use super::{
        hash_key, hash_name, CompressionType, DpaHeader, FORMAT_VERSION, MAGIC,
        SOCKET_FLAG_MIRROR_WITH_PARENT,
    };

    #[test]
    fn layouts_are_stable() {
        assert_eq!(std::mem::size_of::<DpaHeader>(), 64);
        assert_eq!(std::mem::size_of::<super::SpriteRecord>(), 32);
        assert_eq!(std::mem::size_of::<super::SocketRecord>(), 16);
        assert_eq!(std::mem::size_of::<super::CompositeRecord>(), 16);
        assert_eq!(std::mem::size_of::<super::CompositePartRecord>(), 16);
    }

    #[test]
    fn hashes_match_the_engine_contract() {
        assert_eq!(hash_name("hand_r"), 0x55FD_864D);
        assert_eq!(hash_key("solid"), 0x3E71_EA61_12BD_4C94);
        assert_eq!(hash_key("demon_intruder"), 0xD647_A388_1CB3_3F46);
    }

    #[test]
    fn header_roundtrips_and_flags_stay_in_sync() {
        let mut header = DpaHeader::new(512, 512, 32);
        assert_eq!(header.magic, MAGIC);
        assert_eq!(header.version, FORMAT_VERSION);
        assert_eq!(header.flags, 0);
        header.set_compression(CompressionType::Lz4);
        assert_eq!(header.compression_type, CompressionType::Lz4 as u8);
        assert_eq!(
            header.flags & super::FLAG_COMPRESSED,
            super::FLAG_COMPRESSED
        );
        header.set_compression(CompressionType::None);
        assert_eq!(header.flags & super::FLAG_COMPRESSED, 0);

        header.flags |= super::FLAG_PREMULTIPLIED_ALPHA;
        let bytes = bytemuck::bytes_of(&header);
        let decoded: &DpaHeader = bytemuck::from_bytes(bytes);
        assert_eq!(decoded, &header);
    }

    #[test]
    fn socket_mirror_flag_bit_is_stable() {
        assert_eq!(SOCKET_FLAG_MIRROR_WITH_PARENT, 0x01);
    }
}

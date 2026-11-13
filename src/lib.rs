//! Offline Dusk Packed Atlas (`.dpa`) compiler.
//!
//! Pipeline:
//! 1. [`ingest`] reads legacy Lua sprite bitmaps and raw PNG pixel art.
//! 2. [`metadata`] applies optional pivot/socket/composite authoring data.
//! 3. [`pack`] allocates uniform cells and rasterizes the RGBA8 atlas.
//! 4. [`emit`] writes the binary container (optionally LZ4) and Rust constants.

#![deny(clippy::all, clippy::pedantic)]

pub mod emit;
pub mod format;
pub mod glyphs;
pub mod ingest;
pub mod metadata;
pub mod pack;

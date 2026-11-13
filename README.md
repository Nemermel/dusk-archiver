# dusk-archiver

Offline compiler for Dusk's **Packed Atlas** (`.dpa`) container.

The archiver is a standalone crate so content builds do not link `wgpu`,
`winit`, or the runtime ECS. It mirrors the on-disk types from the engine's
`dusk::dpa` module.

## Pipeline

1. **Ingest** `assets/scripts/content.lua` — legacy `sprites` entries with
   `bitmap` rows and optional `ch` fallback glyphs.
2. **Ingest** `assets/raw_image/*.png` — RGBA art, either one 32x32 cell or a
   horizontal animation strip (`N * 32` wide by 32 tall). File stems become
   snake_case keys (`Demon_Intruder.png` → `demon_intruder`).
3. **Apply metadata** from `assets/atlas.toml` (optional): pivots, ASCII
   fallbacks, frame rates, sockets, and composites.
4. **Pack** every frame into a uniform 32x32 cell grid. Cells `0..=127` are
   reserved for the engine's monospace font; content starts at cell `128`.
5. **Emit** `assets/atlas.dpa` with an LZ4-compressed RGBA8 payload by default,
   plus `src/generated/sprites.rs` constants.

Cells and output tables are sorted by content key, so builds are deterministic.

## Usage

Run from this directory (the defaults are relative to it):

```sh
cargo run
```

Common overrides:

```sh
cargo run -- \
  --content ../dusk/assets/scripts/content.lua \
  --raw-dir ../dusk/assets/raw_image \
  --metadata ../dusk/assets/atlas.toml \
  --out ../dusk/assets/atlas.dpa \
  --constants ../dusk/src/generated/sprites.rs
```

Development options:

```sh
# Uncompressed payload, for inspecting the atlas quickly.
cargo run -- --no-compress

# Skip Rust constant generation.
cargo run -- --no-constants

# Force a larger atlas (512, 1024, or 2048); defaults to the smallest that fits.
cargo run -- --atlas-size 1024
```

## Metadata schema

See [`atlas.example.toml`](atlas.example.toml). Every table is optional. If the
file is absent, PNG sprites default to a bottom-center pivot `[16, 32]`, Lua
mask sprites default to `[0, 0]`, and no sockets or composites are emitted.

```toml
[atlas]
size = 512                 # optional: 512, 1024, or 2048
compression = "lz4"        # optional: "lz4" or "none"

[sprite.demon_intruder]
ascii = "d"                # one-byte fallback glyph
pivot = [16, 32]           # pixels from the sprite's top-left
frame_count = 1            # validated against the input
frame_rate_fps = 8
render_mode = "rgba"       # "rgba" or "mask"

[socket.demon_intruder.hand_r]
offset = [12, -16]         # pivot-relative
z = 1                      # signed layer bias
mirror = true              # mirror X when the parent is flipped

[composite.breach_portal_colossus]
grid_size = [3, 3]
interaction_tile = [1, 2]  # optional
footprint = [              # optional collision mask, 1 = blocking
  [0, 1, 0],
  [1, 1, 1],
  [0, 1, 0],
]
parts = [
  { sprite = "void_portal", grid_pos = [0, 0], pixel_offset = [0, 0], z = 31 },
  { sprite = "wall_chitin", grid_pos = [1, 1], pixel_offset = [0, -4], z = 35 },
]
```

Metadata is validated before packing: socket parents and composite parts must
reference known sprites, composite grids and footprints must be consistent, and
part positions must stay inside the grid.

## `.dpa` layout

All offsets are byte offsets from the start of the file:

| Section | Size | Notes |
| :--- | ---: | :--- |
| Header | 64 B | `DPAK`, version 1, flags, dimensions, table offsets |
| Sprite manifest | 32 B × sprites | key hash, cell range, frames/FPS, pivot, sockets, name |
| Sockets | 16 B × sockets | name hash, offset, Z bias, mirror flag |
| Composite headers | 16 B × composites | key hash, grid size, part range |
| Composite parts | 16 B × parts | sprite hash, grid/pixel offset, local Z |
| String table | variable | sprite keys referenced by `name_offset`/`name_len` |
| Padding | 0–3 B | aligns the payload to 4 bytes |
| Pixel payload | variable | LZ4 block or raw RGBA8, `width * height * 4` bytes raw |

The engine-side definitions live in `../dusk/src/dpa.rs`; the compiler's mirror
is in [`src/format.rs`](src/format.rs). Content keys use FNV-1a 64-bit hashes and
socket names use FNV-1a 32-bit hashes.

## Verification

```sh
cargo test
cargo clippy --all-targets -- -D warnings -D clippy::pedantic
```

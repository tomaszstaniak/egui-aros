//! A software renderer for egui's tessellated output: textured, vertex-coloured triangles with
//! premultiplied alpha, drawn into an RGBA frame buffer that the window blits. No GPU is needed
//! (AROS has no wgpu backend), and egui's own feathering does the anti-aliasing.
//!
//! Origin: `graphics/photocraft/photocraft-aros/src/raster.rs` in aros-development-team/ports
//! (commit 8a6faee1, Nick Andrews; as of 685982ed), MIT OR Apache-2.0. Changed since, in the
//! WordCraft and PdfCraft AROS ports: per-kind paint statistics, constant-UV triangles sampled
//! once, and per-row spans that test only the pixels a triangle can cover; then (egui-aros)
//! integer coverage and shading, rectangle fills and repainting of changed tiles only.

use std::collections::HashMap;

use egui::epaint::textures::{TextureFilter, TexturesDelta};
use egui::epaint::{ClippedPrimitive, ImageData, ImageDelta, Primitive, TextureId, Vertex};

struct Texture {
    width: usize,
    height: usize,
    /// Premultiplied sRGBA, row by row.
    pixels: Vec<[u8; 4]>,
    linear: bool,
}

impl Texture {
    #[inline]
    fn texel(&self, x: i32, y: i32) -> [u8; 4] {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.pixels.get(y * self.width + x).copied().unwrap_or([0; 4])
    }

    /// Samples at normalized `(u, v)`: bilinear for textures asking for it, else nearest.
    #[inline]
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let fx = u * self.width as f32 - 0.5;
        let fy = v * self.height as f32 - 0.5;
        if !self.linear {
            let t = self.texel((fx + 0.5).floor() as i32, (fy + 0.5).floor() as i32);
            return [t[0] as f32, t[1] as f32, t[2] as f32, t[3] as f32];
        }
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let (x0, y0) = (x0 as i32, y0 as i32);
        let a = self.texel(x0, y0);
        let b = self.texel(x0 + 1, y0);
        let c = self.texel(x0, y0 + 1);
        let d = self.texel(x0 + 1, y0 + 1);
        let mut out = [0.0f32; 4];
        for i in 0..4 {
            let top = a[i] as f32 * (1.0 - tx) + b[i] as f32 * tx;
            let bottom = c[i] as f32 * (1.0 - tx) + d[i] as f32 * tx;
            out[i] = top * (1.0 - ty) + bottom * ty;
        }
        out
    }
}

/// What `paint` did, by kind of triangle (see [`Kind`]), for the PDFCRAFT_PERF measurement.
#[derive(Clone, Copy, Default)]
pub struct KindStats {
    pub triangles: u32,
    /// Pixels of the triangles' clipped bounding boxes: every one is tested.
    pub visited: u64,
    /// Pixels inside a triangle.
    pub covered: u64,
    pub bilinear: u64,
    pub nearest: u64,
    /// Covered pixels written as a blend (alpha below 255) and as an opaque copy.
    pub blended: u64,
    pub opaque: u64,
    /// Time in runs of consecutive triangles of this kind (two clock reads per run).
    pub us: u64,
}

/// Triangles by what they draw: a user texture (page images, previews), a solid colour (egui's
/// fills sample the font atlas's white texel: all three UVs equal), or the atlas itself (text).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Page = 0,
    Solid = 1,
    Atlas = 2,
}

#[derive(Clone, Copy, Default)]
pub struct PaintStats {
    pub kinds: [KindStats; 3],
    pub clear_us: u64,
    pub runs: u32,
    /// Pixels written by the rectangle fill (also counted in the solid kind).
    pub rect_pixels: u64,
}

pub struct Renderer {
    textures: HashMap<TextureId, Texture>,
    /// Counters of the last `clear` + `paint`; filled only when `measure` is set.
    pub stats: PaintStats,
    pub measure: bool,
    /// RGBA, `width * height * 4` bytes, opaque after `clear`.
    pub frame: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// What each [`TILE`]-pixel tile of `frame` shows, as a hash of everything painted into it by
    /// the last [`Renderer::paint_changed`]; empty when `frame` is not known to match (new size,
    /// or painted by `clear` and `paint`).
    tiles: Vec<u64>,
    /// Counts a texture's uploads, so that tiles showing it change when its pixels do.
    texture_versions: HashMap<TextureId, u64>,
    uploads: u64,
    /// Reused between frames: per triangle, the tiles its clipped bounding box touches.
    spans: Vec<[u16; 4]>,
    /// Reused between frames: the vertices' snapped pixel positions, mesh after mesh.
    fixed: Vec<[i32; 2]>,
}

/// Side of the squares `paint_changed` compares and repaints, in pixels.
pub const TILE: usize = 64;

/// What [`Renderer::paint_changed`] did.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Changed {
    pub tiles: u32,
    pub repainted: u32,
    /// The pixel box around the repainted tiles: x0, y0, x1, y1 (max exclusive).
    pub rect: Option<[u32; 4]>,
}

impl Renderer {
    pub fn new(width: usize, height: usize) -> Self {
        let mut r = Self {
            textures: HashMap::new(),
            stats: PaintStats::default(),
            measure: false,
            frame: Vec::new(),
            width: 0,
            height: 0,
            tiles: Vec::new(),
            texture_versions: HashMap::new(),
            uploads: 0,
            spans: Vec::new(),
            fixed: Vec::new(),
        };
        r.resize(width, height);
        r
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.frame = vec![0; width * height * 4];
            self.tiles.clear();
        }
    }

    pub fn stride(&self) -> usize {
        self.width * 4
    }

    pub fn clear(&mut self, rgb: [u8; 3]) {
        // The frame no longer shows what the tile hashes describe.
        self.tiles.clear();
        let started = self.measure.then(std::time::Instant::now);
        for px in self.frame.chunks_exact_mut(4) {
            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        self.stats = PaintStats::default();
        if let Some(t) = started {
            self.stats.clear_us = t.elapsed().as_micros() as u64;
        }
    }

    /// Applies the frame's texture uploads (before painting).
    pub fn update_textures(&mut self, delta: &TexturesDelta) {
        for (id, deltas) in &delta.set {
            self.uploads += 1;
            self.texture_versions.insert(*id, self.uploads);
            for d in deltas {
                self.set_texture(*id, d);
            }
        }
    }

    /// Frees every texture (when the window closes).
    pub fn free_all(&mut self) {
        self.textures.clear();
    }

    /// Frees the frame's released textures (after painting). The caller clears the delta
    /// afterwards: egui 0.37 checks in debug builds that no delta is dropped unapplied.
    pub fn free_textures(&mut self, delta: &TexturesDelta) {
        for id in &delta.free {
            self.textures.remove(id);
            self.texture_versions.remove(id);
        }
    }

    fn set_texture(&mut self, id: TextureId, delta: &ImageDelta) {
        let ImageData::Color(image) = &delta.image;
        let [w, h] = image.size;
        let linear = delta.options.magnification == TextureFilter::Linear;
        match delta.pos {
            None => {
                self.textures.insert(id, Texture { width: w, height: h, pixels: image.pixels.iter().map(|c| c.to_array()).collect(), linear });
            }
            Some([x, y]) => {
                let Some(t) = self.textures.get_mut(&id) else { return };
                for row in 0..h {
                    let ty = y + row;
                    if ty >= t.height {
                        break;
                    }
                    for col in 0..w {
                        let tx = x + col;
                        if tx >= t.width {
                            break;
                        }
                        if let (Some(dst), Some(src)) = (t.pixels.get_mut(ty * t.width + tx), image.pixels.get(row * w + col)) {
                            *dst = src.to_array();
                        }
                    }
                }
            }
        }
    }

    /// `clear` and `paint` for the parts of the frame that differ from what it already shows:
    /// the frame is divided into [`TILE`]-pixel tiles, each tile's content is identified by a
    /// hash of the triangles (positions, colours, texture and its upload count, clip) that reach
    /// it, in order, and only tiles whose hash changed since the previous call are cleared and
    /// painted again. A frame that repeats the previous one costs the hashing only. With
    /// `measure` set, or after a resize, everything is painted.
    pub fn paint_changed(&mut self, rgb: [u8; 3], primitives: &[ClippedPrimitive], pixels_per_point: f32) -> Changed {
        let (tw, th) = (self.width.div_ceil(TILE), self.height.div_ceil(TILE));
        let total = (tw * th) as u32;
        let whole = Some([0, 0, self.width as u32, self.height as u32]);
        if self.measure || tw > u16::MAX as usize || th > u16::MAX as usize {
            self.clear(rgb);
            self.paint(primitives, pixels_per_point);
            return Changed { tiles: total, repainted: total, rect: whole };
        }
        const K: u64 = 0x9e37_79b9_7f4a_7c15;
        let mix = |h: u64, v: u64| (h.rotate_left(5) ^ v).wrapping_mul(K);
        let seed = mix(mix(u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 0]) as u64, pixels_per_point.to_bits() as u64), self.width as u64);
        let mut now = vec![seed; tw * th];
        let mut spans = std::mem::take(&mut self.spans);
        let mut fixed = std::mem::take(&mut self.fixed);
        spans.clear();
        fixed.clear();
        const NONE: [u16; 4] = [u16::MAX; 4];
        let (fw, fh) = (self.width as i32, self.height as i32);
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let clip = clip_box(p, pixels_per_point, fw, fh);
            let empty = clip[2] <= clip[0] || clip[3] <= clip[1];
            let first = fixed.len();
            fixed.extend(mesh.vertices.iter().map(|v| [snap(v.pos.x * pixels_per_point), snap(v.pos.y * pixels_per_point)]));
            let fixed = &fixed[first..];
            let texture = match mesh.texture_id {
                TextureId::Managed(n) => n << 1,
                TextureId::User(n) => (n << 1) | 1,
            };
            let version = self.texture_versions.get(&mesh.texture_id).copied().unwrap_or(0);
            let base = mix(mix(mix(mix(texture, version), (clip[0] as u64) << 32 | clip[1] as u64), (clip[2] as u64) << 32 | clip[3] as u64), 1);
            for tri in mesh.indices.chunks_exact(3) {
                let [ia, ib, ic] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
                let (Some(a), Some(b), Some(c)) = (mesh.vertices.get(ia), mesh.vertices.get(ib), mesh.vertices.get(ic)) else {
                    spans.push(NONE);
                    continue;
                };
                // The pixels `triangle` can write.
                let Some([min_x, min_y, max_x, max_y]) = (if empty { None } else { pixel_box([fixed[ia], fixed[ib], fixed[ic]], clip) }) else {
                    spans.push(NONE);
                    continue;
                };
                let span = [min_x as usize / TILE, min_y as usize / TILE, (max_x as usize - 1) / TILE, (max_y as usize - 1) / TILE];
                let mut h = base;
                for v in [a, b, c] {
                    h = mix(h, (v.pos.x.to_bits() as u64) << 32 | v.pos.y.to_bits() as u64);
                    h = mix(h, (v.uv.x.to_bits() as u64) << 32 | v.uv.y.to_bits() as u64);
                    h = mix(h, u32::from_le_bytes(v.color.to_array()) as u64);
                }
                for ty in span[1]..=span[3] {
                    for t in &mut now[ty * tw + span[0]..=ty * tw + span[2]] {
                        *t = mix(*t, h);
                    }
                }
                spans.push([span[0] as u16, span[1] as u16, span[2] as u16, span[3] as u16]);
            }
        }
        let known = self.tiles.len() == now.len();
        let dirty: Vec<bool> = now.iter().enumerate().map(|(i, h)| !known || self.tiles[i] != *h).collect();
        let repainted = dirty.iter().filter(|d| **d).count() as u32;
        if repainted == 0 {
            self.spans = spans;
            self.fixed = fixed;
            return Changed { tiles: total, repainted: 0, rect: None };
        }
        // Per tile row, the runs of changed tiles as pixel ranges; cleared, then painted.
        let mut runs: Vec<Vec<(i32, i32)>> = vec![Vec::new(); th];
        let mut rect = [u32::MAX, u32::MAX, 0u32, 0u32];
        for ty in 0..th {
            let mut tx = 0;
            while tx < tw {
                if dirty[ty * tw + tx] {
                    let from = tx;
                    while tx < tw && dirty[ty * tw + tx] {
                        tx += 1;
                    }
                    runs[ty].push(((from * TILE) as i32, (tx * TILE).min(self.width) as i32));
                } else {
                    tx += 1;
                }
            }
            let (ya, yb) = (ty * TILE, ((ty + 1) * TILE).min(self.height));
            for &(xa, xb) in &runs[ty] {
                rect = [rect[0].min(xa as u32), rect[1].min(ya as u32), rect[2].max(xb as u32), rect[3].max(yb as u32)];
                for y in ya..yb {
                    let row = y * self.width;
                    if let Some(px) = self.frame.get_mut((row + xa as usize) * 4..(row + xb as usize) * 4) {
                        for px in px.chunks_exact_mut(4) {
                            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                        }
                    }
                }
            }
        }
        let (mut n, mut first) = (0usize, 0usize);
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let clip = clip_box(p, pixels_per_point, fw, fh);
            let texture = self.textures.get(&mesh.texture_id);
            let fixed = &fixed[first..first + mesh.vertices.len()];
            let count = mesh.indices.len() / 3;
            let mut i = 0;
            while i < count {
                let span = spans[n + i];
                i += 1;
                if span == NONE {
                    continue;
                }
                let [sx0, sy0, sx1, sy1] = [span[0] as usize, span[1] as usize, span[2] as usize, span[3] as usize];
                // Most triangles lie in one tile.
                if sx0 == sx1 && sy0 == sy1 && !dirty[sy0 * tw + sx0] {
                    continue;
                }
                let tri = &mesh.indices[(i - 1) * 3..i * 3];
                let [ia, ib, ic] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
                // The other half of a rectangle has the same tiles: both are done here.
                let quad = if i < count && spans[n + i] == span {
                    opaque_quad(&mesh.vertices, fixed, tri, &mesh.indices[i * 3..i * 3 + 3], texture)
                } else {
                    None
                };
                if quad.is_some() {
                    i += 1;
                }
                for ty in sy0..=sy1 {
                    let (ya, yb) = ((ty * TILE) as i32, ((ty + 1) * TILE) as i32);
                    for &(xa, xb) in &runs[ty] {
                        if xb <= (sx0 * TILE) as i32 || xa >= ((sx1 + 1) * TILE) as i32 {
                            continue;
                        }
                        let part = [clip[0].max(xa), clip[1].max(ya), clip[2].min(xb), clip[3].min(yb)];
                        if part[0] >= part[2] || part[1] >= part[3] {
                            continue;
                        }
                        match &quad {
                            Some(q) => {
                                fill_rect(&mut self.frame, self.width, part, q);
                            }
                            None => triangle(
                                &mut self.frame,
                                self.width,
                                part,
                                texture,
                                [fixed[ia], fixed[ib], fixed[ic]],
                                [&mesh.vertices[ia], &mesh.vertices[ib], &mesh.vertices[ic]],
                                None,
                            ),
                        }
                    }
                }
            }
            n += count;
            first += mesh.vertices.len();
        }
        self.tiles = now;
        self.spans = spans;
        self.fixed = fixed;
        Changed { tiles: total, repainted, rect: Some(rect) }
    }

    pub fn paint(&mut self, primitives: &[ClippedPrimitive], pixels_per_point: f32) {
        let mut fixed = std::mem::take(&mut self.fixed);
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let clip = clip_box(p, pixels_per_point, self.width as i32, self.height as i32);
            if clip[2] <= clip[0] || clip[3] <= clip[1] {
                continue;
            }
            fixed.clear();
            fixed.extend(mesh.vertices.iter().map(|v| [snap(v.pos.x * pixels_per_point), snap(v.pos.y * pixels_per_point)]));
            let texture = self.textures.get(&mesh.texture_id);
            let user_texture = matches!(mesh.texture_id, TextureId::User(_));
            let measure = self.measure;
            // The current run of same-kind triangles and when it started.
            let mut run: Option<(Kind, std::time::Instant)> = None;
            let mut tris = mesh.indices.chunks_exact(3).peekable();
            while let Some(tri) = tris.next() {
                // Two triangles that are the halves of one opaque, single-colour, axis-aligned
                // rectangle (panel and cell backgrounds: most of what a frame paints) are filled
                // as rows, with the pixels the two triangles would cover.
                if let Some(next) = tris.peek() {
                    if let Some(q) = opaque_quad(&mesh.vertices, &fixed, tri, next, texture) {
                        let started = measure.then(std::time::Instant::now);
                        let n = fill_rect(&mut self.frame, self.width, clip, &q);
                        if let Some(t) = started {
                            // Closes a run of another kind; the fill is timed on its own.
                            if let Some((k, t0)) = run.take() {
                                self.stats.kinds[k as usize].us += (t - t0).as_micros() as u64;
                            }
                            let st = &mut self.stats.kinds[Kind::Solid as usize];
                            st.triangles += 2;
                            st.visited += n;
                            st.covered += n;
                            st.opaque += n;
                            st.us += t.elapsed().as_micros() as u64;
                            self.stats.rect_pixels += n;
                        }
                        tris.next();
                        continue;
                    }
                }
                let [ia, ib, ic] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
                let (Some(a), Some(b), Some(c)) = (mesh.vertices.get(ia), mesh.vertices.get(ib), mesh.vertices.get(ic)) else {
                    continue;
                };
                let stats = if measure {
                    let kind = if user_texture || mesh.texture_id != TextureId::Managed(0) {
                        Kind::Page
                    } else if a.uv == b.uv && b.uv == c.uv {
                        Kind::Solid
                    } else {
                        Kind::Atlas
                    };
                    if run.is_none_or(|(k, _)| k != kind) {
                        let now = std::time::Instant::now();
                        if let Some((k, t)) = run {
                            self.stats.kinds[k as usize].us += (now - t).as_micros() as u64;
                        }
                        self.stats.runs += 1;
                        run = Some((kind, now));
                    }
                    Some(&mut self.stats.kinds[kind as usize])
                } else {
                    None
                };
                triangle(&mut self.frame, self.width, clip, texture, [fixed[ia], fixed[ib], fixed[ic]], [a, b, c], stats);
            }
            if let Some((k, t)) = run {
                self.stats.kinds[k as usize].us += t.elapsed().as_micros() as u64;
            }
        }
        self.fixed = fixed;
    }
}

/// The pixel clip box of a primitive within a `width` x `height` frame (max exclusive).
fn clip_box(p: &ClippedPrimitive, ppp: f32, width: i32, height: i32) -> [i32; 4] {
    let c = p.clip_rect;
    [
        ((c.min.x * ppp).floor() as i32).max(0),
        ((c.min.y * ppp).floor() as i32).max(0),
        ((c.max.x * ppp).ceil() as i32).min(width),
        ((c.max.y * ppp).ceil() as i32).min(height),
    ]
}

/// Positions are snapped to 1/256 pixel and coverage is decided in integers: exact, the same
/// whatever part of a triangle is drawn, and without floating point in the pixel loops.
const SUB_BITS: u32 = 8;
const ONE: i64 = 1 << SUB_BITS;
const HALF: i64 = ONE / 2;
/// Fraction bits of the interpolated colour channels.
const COLOR_BITS: u32 = 20;
/// Fraction bits of the interpolated texel coordinates.
const UV_BITS: u32 = 16;

/// A coordinate in pixels as 1/256 pixel; far-away and non-finite values are pulled in.
#[inline]
fn snap(v: f32) -> i32 {
    const LIMIT: f32 = 262_144.0;
    if v.is_nan() {
        return 0;
    }
    // Rounded to the nearest by the cast's truncation (no library call on plain x86-64).
    let v = v.clamp(-LIMIT, LIMIT) * ONE as f32;
    (v + if v >= 0.0 { 0.5 } else { -0.5 }) as i32
}

/// The first pixel whose centre is at or after `edge` (1/256 pixel): a span's first pixel for
/// a left or top edge, and the pixel after its last for a right or bottom edge.
#[inline]
fn first_pixel(edge: i64) -> i64 {
    (edge + HALF - 1) >> SUB_BITS
}

#[inline]
fn div255(x: u32) -> u32 {
    x / 255
}

/// An opaque axis-aligned rectangle (edges in 1/256 pixel) and its colour.
struct Quad {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    rgba: [u8; 4],
}

/// The colour of a vertex colour times a texel, as `triangle` computes it.
#[inline]
fn modulate(c: [u8; 4], s: [u8; 4]) -> [u8; 4] {
    [
        div255(c[0] as u32 * s[0] as u32) as u8,
        div255(c[1] as u32 * s[1] as u32) as u8,
        div255(c[2] as u32 * s[2] as u32) as u8,
        div255(c[3] as u32 * s[3] as u32) as u8,
    ]
}

/// The texel a triangle with three equal UVs shows everywhere.
#[inline]
fn uniform_texel(texture: Option<&Texture>, uv: egui::Pos2) -> [u8; 4] {
    match texture {
        Some(t) => {
            let s = t.sample(uv.x, uv.y);
            [(s[0] + 0.5) as u8, (s[1] + 0.5) as u8, (s[2] + 0.5) as u8, (s[3] + 0.5) as u8]
        }
        None => [255; 4],
    }
}

/// The rectangle two consecutive triangles make, when they are its two halves, of one colour,
/// sample one texel (egui's fills) and the result is opaque. `fixed` holds the vertices'
/// snapped pixel positions.
fn opaque_quad(vertices: &[Vertex], fixed: &[[i32; 2]], t0: &[u32], t1: &[u32], texture: Option<&Texture>) -> Option<Quad> {
    let v = |i: u32| vertices.get(i as usize);
    let (a, b, c) = (v(t0[0])?, v(t0[1])?, v(t0[2])?);
    let (d, e, f) = (v(t1[0])?, v(t1[1])?, v(t1[2])?);
    let all = [a, b, c, d, e, f];
    if all.iter().any(|q| q.color != a.color || q.uv != a.uv) || a.color.a() != 255 {
        return None;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (a.pos.x, a.pos.x, a.pos.y, a.pos.y);
    for q in all {
        x0 = x0.min(q.pos.x);
        x1 = x1.max(q.pos.x);
        y0 = y0.min(q.pos.y);
        y1 = y1.max(q.pos.y);
    }
    if !(x0 < x1 && y0 < y1) {
        return None;
    }
    // Every vertex is a corner, each triangle has three different corners, and together they
    // have all four: the two halves of the rectangle, split along either diagonal.
    let corner = |q: &Vertex| -> Option<u8> {
        let x = if q.pos.x == x0 { 0 } else if q.pos.x == x1 { 1 } else { return None };
        let y = if q.pos.y == y0 { 0 } else if q.pos.y == y1 { 2 } else { return None };
        Some(1 << (x + y))
    };
    let (ca, cb, cc) = (corner(a)?, corner(b)?, corner(c)?);
    let (cd, ce, cf) = (corner(d)?, corner(e)?, corner(f)?);
    let (m0, m1) = (ca | cb | cc, cd | ce | cf);
    if m0.count_ones() != 3 || m1.count_ones() != 3 || m0 | m1 != 0b1111 || m0 == m1 {
        return None;
    }
    let rgba = modulate(a.color.to_array(), uniform_texel(texture, a.uv));
    if rgba[3] != 255 {
        return None;
    }
    let (mut fx0, mut fx1, mut fy0, mut fy1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for i in t0.iter().chain(t1) {
        let p = fixed.get(*i as usize)?;
        fx0 = fx0.min(p[0]);
        fx1 = fx1.max(p[0]);
        fy0 = fy0.min(p[1]);
        fy1 = fy1.max(p[1]);
    }
    Some(Quad { x0: fx0 as i64, y0: fy0 as i64, x1: fx1 as i64, y1: fy1 as i64, rgba })
}

/// Fills the pixels whose centres lie inside the rectangle, left and top edges included, right
/// and bottom excluded (the rule `triangle` follows), within the clip box. Returns the count.
fn fill_rect(frame: &mut [u8], width: usize, clip: [i32; 4], q: &Quad) -> u64 {
    let xa = first_pixel(q.x0).max(clip[0] as i64);
    let xb = first_pixel(q.x1).min(clip[2] as i64);
    let ya = first_pixel(q.y0).max(clip[1] as i64);
    let yb = first_pixel(q.y1).min(clip[3] as i64);
    if xa >= xb || ya >= yb {
        return 0;
    }
    for py in ya..yb {
        let start = (py as usize * width + xa as usize) * 4;
        let end = (py as usize * width + xb as usize) * 4;
        if let Some(row) = frame.get_mut(start..end) {
            for px in row.chunks_exact_mut(4) {
                px.copy_from_slice(&q.rgba);
            }
        }
    }
    (xb - xa) as u64 * (yb - ya) as u64
}

/// The clipped pixel box (max exclusive) holding every pixel centre a triangle with these
/// snapped corners can cover; `None` when there is none.
#[inline]
fn pixel_box(p: [[i32; 2]; 3], clip: [i32; 4]) -> Option<[i32; 4]> {
    let min_x = first_pixel(p[0][0].min(p[1][0]).min(p[2][0]) as i64).max(clip[0] as i64);
    let max_x = (((p[0][0].max(p[1][0]).max(p[2][0]) as i64 - HALF) >> SUB_BITS) + 1).min(clip[2] as i64);
    let min_y = first_pixel(p[0][1].min(p[1][1]).min(p[2][1]) as i64).max(clip[1] as i64);
    let max_y = (((p[0][1].max(p[1][1]).max(p[2][1]) as i64 - HALF) >> SUB_BITS) + 1).min(clip[3] as i64);
    (min_x < max_x && min_y < max_y).then_some([min_x as i32, min_y as i32, max_x as i32, max_y as i32])
}

/// What a triangle's pixels are made of.
enum Shade {
    /// One colour (premultiplied).
    Flat([u8; 4]),
    /// Colour interpolated between the vertices: value at the box's first pixel, change per
    /// pixel to the right and per row, with `COLOR_BITS` fraction bits.
    Gradient { at: [i64; 4], dx: [i64; 4], dy: [i64; 4] },
}

/// Texel coordinates across a triangle (`UV_BITS` fraction bits, texel units).
struct Texels<'a> {
    texture: &'a Texture,
    at: [i64; 2],
    dx: [i64; 2],
    dy: [i64; 2],
}

impl Texture {
    /// The texel at fixed-point texel coordinates, as `sample` gives it: nearest or bilinear.
    #[inline]
    fn sample_fixed(&self, u: i64, v: i64) -> [u8; 4] {
        if !self.linear {
            return self.texel((u >> UV_BITS) as i32, (v >> UV_BITS) as i32);
        }
        let (fu, fv) = (u - (1 << (UV_BITS - 1)), v - (1 << (UV_BITS - 1)));
        let (x0, y0) = ((fu >> UV_BITS) as i32, (fv >> UV_BITS) as i32);
        let tx = ((fu >> (UV_BITS - 8)) & 0xff) as u32;
        let ty = ((fv >> (UV_BITS - 8)) & 0xff) as u32;
        let (a, b, c, d) = (self.texel(x0, y0), self.texel(x0 + 1, y0), self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1));
        let mut out = [0u8; 4];
        for i in 0..4 {
            let top = a[i] as u32 * (256 - tx) + b[i] as u32 * tx;
            let bottom = c[i] as u32 * (256 - tx) + d[i] as u32 * tx;
            out[i] = ((top * (256 - ty) + bottom * ty + 32768) >> 16) as u8;
        }
        out
    }
}

/// Writes one premultiplied pixel over `dst`; returns whether it was an opaque copy.
#[inline]
fn put(dst: &mut [u8], src: [u8; 4]) -> bool {
    let a = src[3] as u32;
    if a == 255 {
        dst.copy_from_slice(&[src[0], src[1], src[2], 255]);
        return true;
    }
    let keep = 255 - a;
    for i in 0..4 {
        dst[i] = (src[i] as u32 + div255(dst[i] as u32 * keep + 127)).min(255) as u8;
    }
    false
}

/// Rasterizes one triangle into `frame` within the clip box (pixel units, max exclusive).
/// `p` holds the corners' snapped pixel positions ([`snap`]). A pixel is covered when its
/// centre is inside; a centre exactly on an edge belongs to one triangle only (the top-left
/// rule of D3D and GL rasterizers, y down): to the one whose edge is a left edge (the edge
/// function grows to the right) or a top edge (horizontal, growing downwards). Without it, a
/// pixel on the edge two triangles share was drawn by both, which shows as a line where they
/// are translucent (the diagonal of every image quad at integer positions).
#[allow(clippy::too_many_arguments)]
fn triangle(frame: &mut [u8], width: usize, clip: [i32; 4], texture: Option<&Texture>, p: [[i32; 2]; 3], v: [&Vertex; 3], stats: Option<&mut KindStats>) {
    let [ax, ay] = [p[0][0] as i64, p[0][1] as i64];
    let [bx, by] = [p[1][0] as i64, p[1][1] as i64];
    let [cx, cy] = [p[2][0] as i64, p[2][1] as i64];
    let mut area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
    if area == 0 {
        return;
    }
    let Some([min_x, min_y, max_x, max_y]) = pixel_box(p, clip) else { return };
    // The three edge functions at the centre of the box's first pixel, and how each changes
    // per pixel to the right and per row down. Inside: all three positive (or zero on a left
    // or top edge) once normalized to the triangle's winding.
    let (px, py) = (min_x as i64 * ONE + HALF, min_y as i64 * ONE + HALF);
    let mut e = [
        (bx - px) * (cy - py) - (by - py) * (cx - px),
        (cx - px) * (ay - py) - (cy - py) * (ax - px),
        (ax - px) * (by - py) - (ay - py) * (bx - px),
    ];
    let mut sx = [(by - cy) * ONE, (cy - ay) * ONE, (ay - by) * ONE];
    let mut sy = [(cx - bx) * ONE, (ax - cx) * ONE, (bx - ax) * ONE];
    if area < 0 {
        area = -area;
        for i in 0..3 {
            e[i] = -e[i];
            sx[i] = -sx[i];
            sy[i] = -sy[i];
        }
    }
    // The edge functions at the centre of pixel (0, 0): interpolated values start from there, so
    // that they do not depend on which part of the triangle the clip box leaves.
    let origin = [0, 1, 2].map(|i| e[i] - sx[i] * min_x as i64 - sy[i] * min_y as i64);
    let (ox, oy) = (min_x as i64, min_y as i64);
    // The least value of each edge function that counts as inside.
    let least = [0, 1, 2].map(|i| if sx[i] > 0 || (sx[i] == 0 && sy[i] > 0) { 0i64 } else { 1 });

    let colors = [v[0].color.to_array(), v[1].color.to_array(), v[2].color.to_array()];
    let same_uv = v[0].uv == v[1].uv && v[1].uv == v[2].uv;
    // A triangle whose three UVs are equal samples one point of its texture at every pixel
    // (egui's fills use the atlas's white texel): sampled once, folded into the colours.
    let (colors, texels) = match texture {
        Some(t) if !same_uv => {
            // Texel coordinates are linear across the triangle; set up in floating point once.
            let (w, h) = (t.width as f64, t.height as f64);
            let scale = (1u64 << UV_BITS) as f64 / area as f64;
            let lin = |f: [f64; 3], k: [i64; 3]| ((f[0] * k[0] as f64 + f[1] * k[1] as f64 + f[2] * k[2] as f64) * scale) as i64;
            let u = [v[0].uv.x as f64 * w, v[1].uv.x as f64 * w, v[2].uv.x as f64 * w];
            let vv = [v[0].uv.y as f64 * h, v[1].uv.y as f64 * h, v[2].uv.y as f64 * h];
            let (dx, dy) = ([lin(u, sx), lin(vv, sx)], [lin(u, sy), lin(vv, sy)]);
            let at = [0, 1].map(|i| [lin(u, origin), lin(vv, origin)][i].wrapping_add(dx[i].wrapping_mul(ox)).wrapping_add(dy[i].wrapping_mul(oy)));
            (colors, Some(Texels { texture: t, at, dx, dy }))
        }
        _ => {
            let s = uniform_texel(texture, v[0].uv);
            (colors.map(|c| modulate(c, s)), None)
        }
    };
    let shade = if colors[0] == colors[1] && colors[1] == colors[2] {
        Shade::Flat(colors[0])
    } else {
        // The change per pixel: the sum of colour times each edge function's step, over the
        // area. 64-bit unless the triangle is huge.
        let small = sx.iter().chain(&sy).all(|k| k.abs() < 1 << 33);
        let lin = |ch: usize, k: [i64; 3]| -> i64 {
            if small {
                let sum = colors[0][ch] as i64 * k[0] + colors[1][ch] as i64 * k[1] + colors[2][ch] as i64 * k[2];
                (sum << COLOR_BITS) / area
            } else {
                let sum = colors[0][ch] as i128 * k[0] as i128 + colors[1][ch] as i128 * k[1] as i128 + colors[2][ch] as i128 * k[2] as i128;
                ((sum << COLOR_BITS) / area as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64
            }
        };
        let all = |k: [i64; 3]| [lin(0, k), lin(1, k), lin(2, k), lin(3, k)];
        let (dx, dy) = (all(sx), all(sy));
        // A sliver's colour changes faster than the numbers hold; it covers next to nothing.
        const STEEP: i64 = 1 << 34;
        if dx.iter().chain(&dy).any(|d| d.abs() > STEEP) {
            let mean = |ch: usize| ((colors[0][ch] as u32 + colors[1][ch] as u32 + colors[2][ch] as u32) / 3) as u8;
            Shade::Flat([mean(0), mean(1), mean(2), mean(3)])
        } else {
            // From the first corner, where the colour is that corner's, to the box's first pixel
            // centre: the same whatever part of the triangle the clip box leaves.
            let (px_off, py_off) = (px - ax, py - ay);
            let at = [0, 1, 2, 3].map(|i| ((colors[0][i] as i64) << COLOR_BITS) + ((dx[i] * px_off + dy[i] * py_off) >> SUB_BITS));
            Shade::Gradient { at, dx, dy }
        }
    };
    if let (Shade::Flat([_, _, _, 0]), None) = (&shade, &texels) {
        return;
    }

    let len = (max_x - min_x) as i64;
    let (mut covered, mut sampled, mut blended, mut opaque) = (0u64, 0u64, 0u64, 0u64);
    let (mut shade_at, mut uv_at) = (
        match &shade {
            Shade::Gradient { at, .. } => *at,
            Shade::Flat(_) => [0; 4],
        },
        texels.as_ref().map_or([0; 2], |t| t.at),
    );
    for row_y in min_y..max_y {
        // The pixels of this row inside all three edges: each function is linear along the
        // row, so it is at or above its least value from some pixel on, or up to some pixel.
        let (mut lo, mut hi) = (0i64, len);
        if len <= 16 {
            // Narrow boxes (most triangles): walk the row instead of dividing.
            let mut w = e;
            while lo < len && !(w[0] >= least[0] && w[1] >= least[1] && w[2] >= least[2]) {
                w = [w[0] + sx[0], w[1] + sx[1], w[2] + sx[2]];
                lo += 1;
            }
            hi = lo;
            while hi < len && w[0] >= least[0] && w[1] >= least[1] && w[2] >= least[2] {
                w = [w[0] + sx[0], w[1] + sx[1], w[2] + sx[2]];
                hi += 1;
            }
        } else {
            for i in 0..3 {
                let need = least[i] - e[i];
                if sx[i] > 0 {
                    lo = lo.max(-((-need).div_euclid(sx[i])));
                } else if sx[i] < 0 {
                    hi = hi.min((-need).div_euclid(-sx[i]) + 1);
                } else if need > 0 {
                    hi = 0;
                }
            }
        }
        if lo < hi {
            let n = (hi - lo) as u64;
            covered += n;
            let first = (row_y as usize * width + (min_x as i64 + lo) as usize) * 4;
            if let Some(row) = frame.get_mut(first..first + n as usize * 4) {
                match (&shade, &texels) {
                    (Shade::Flat(c), None) => {
                        if c[3] == 255 {
                            opaque += n;
                            for px in row.chunks_exact_mut(4) {
                                px.copy_from_slice(c);
                            }
                        } else {
                            blended += n;
                            for px in row.chunks_exact_mut(4) {
                                put(px, *c);
                            }
                        }
                    }
                    (Shade::Gradient { dx, .. }, None) => {
                        let mut c = [0, 1, 2, 3].map(|i| shade_at[i] + dx[i] * lo);
                        for px in row.chunks_exact_mut(4) {
                            let src = c.map(|x| ((x + (1 << (COLOR_BITS - 1))) >> COLOR_BITS).clamp(0, 255) as u8);
                            if src[3] != 0 {
                                if put(px, src) {
                                    opaque += 1;
                                } else {
                                    blended += 1;
                                }
                            }
                            for i in 0..4 {
                                c[i] += dx[i];
                            }
                        }
                    }
                    (shade, Some(t)) => {
                        sampled += n;
                        let (mut u, mut w) = (uv_at[0] + t.dx[0] * lo, uv_at[1] + t.dx[1] * lo);
                        let (mut c, cdx) = match shade {
                            Shade::Gradient { dx, .. } => ([0, 1, 2, 3].map(|i| shade_at[i] + dx[i] * lo), *dx),
                            Shade::Flat(_) => ([0; 4], [0; 4]),
                        };
                        for px in row.chunks_exact_mut(4) {
                            let color = match shade {
                                Shade::Flat(c) => *c,
                                Shade::Gradient { .. } => c.map(|x| ((x + (1 << (COLOR_BITS - 1))) >> COLOR_BITS).clamp(0, 255) as u8),
                            };
                            let src = modulate(color, t.texture.sample_fixed(u, w));
                            if src[3] != 0 {
                                if put(px, src) {
                                    opaque += 1;
                                } else {
                                    blended += 1;
                                }
                            }
                            u = u.wrapping_add(t.dx[0]);
                            w = w.wrapping_add(t.dx[1]);
                            for i in 0..4 {
                                c[i] += cdx[i];
                            }
                        }
                    }
                }
            }
        }
        for i in 0..3 {
            e[i] += sy[i];
        }
        if let Shade::Gradient { dy, .. } = &shade {
            for i in 0..4 {
                shade_at[i] += dy[i];
            }
        }
        if let Some(t) = &texels {
            uv_at[0] = uv_at[0].wrapping_add(t.dy[0]);
            uv_at[1] = uv_at[1].wrapping_add(t.dy[1]);
        }
    }
    if let Some(st) = stats {
        st.triangles += 1;
        st.visited += covered;
        st.covered += covered;
        if texture.is_some_and(|t| t.linear) {
            st.bilinear += sampled;
        } else {
            st.nearest += sampled;
        }
        st.blended += blended;
        st.opaque += opaque;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// egui 0.37 sends a texture's full image and later updates of a part of it (`pos`), possibly
    /// several for one texture in one frame, in order; a part outside the texture is cut off.
    #[test]
    fn partial_texture_updates_apply_in_order() {
        use egui::epaint::textures::TextureOptions;
        use egui::{Color32, ColorImage};
        let id = TextureId::Managed(7);
        let mut renderer = Renderer::new(1, 1);
        let mut delta = TexturesDelta::default();
        let full = ColorImage::new([4, 3], vec![Color32::RED; 12]);
        delta.set.entry(id).or_default().push(ImageDelta::full(full, TextureOptions::NEAREST));
        renderer.update_textures(&delta);
        delta.clear();
        let mut delta = TexturesDelta::default();
        let part = |c, w, h| ColorImage::new([w, h], vec![c; w * h]);
        delta.set.entry(id).or_default().push(ImageDelta::partial([1, 1], part(Color32::BLUE, 2, 1), TextureOptions::NEAREST));
        delta.set.entry(id).or_default().push(ImageDelta::partial([2, 1], part(Color32::GREEN, 1, 1), TextureOptions::NEAREST));
        delta.set.entry(id).or_default().push(ImageDelta::partial([3, 2], part(Color32::WHITE, 3, 3), TextureOptions::NEAREST));
        renderer.update_textures(&delta);
        delta.clear();
        let t = &renderer.textures[&id];
        let at = |x: usize, y: usize| Color32::from_rgba_premultiplied(t.pixels[y * 4 + x][0], t.pixels[y * 4 + x][1], t.pixels[y * 4 + x][2], t.pixels[y * 4 + x][3]);
        assert_eq!((t.width, t.height), (4, 3));
        assert_eq!(at(0, 1), Color32::RED);
        assert_eq!(at(1, 1), Color32::BLUE);
        assert_eq!(at(2, 1), Color32::GREEN);
        assert_eq!(at(3, 1), Color32::RED);
        assert_eq!(at(3, 2), Color32::WHITE);
        assert_eq!(at(2, 2), Color32::RED);
    }

    fn tri(frame: &mut [u8], w: usize, clip: [i32; 4], a: &Vertex, b: &Vertex, c: &Vertex) {
        let f = |v: &Vertex| [snap(v.pos.x), snap(v.pos.y)];
        triangle(frame, w, clip, None, [f(a), f(b), f(c)], [a, b, c], None);
    }

    /// The row spans are exactly the pixels whose centres pass the three edge tests (with the
    /// top-left rule), for large, small, thin and sub-pixel triangles of both windings.
    #[test]
    fn spans_are_the_pixels_inside() {
        use egui::epaint::{Color32, Pos2};
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move |range: f32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 1_000_000) as f32 / 1_000_000.0 * range
        };
        let (w, h) = (128usize, 128usize);
        let mut checked = 0u64;
        for case in 0..6000 {
            let scale = [120.0, 40.0, 4.0, 1.5][case % 4];
            let (ox, oy) = (rnd(110.0) - 5.0, rnd(110.0) - 5.0);
            // Every third case on a 1/2 pixel grid: centres exactly on edges and corners.
            let mut c = |o: f32| if case % 3 == 0 { ((o + rnd(scale)) * 2.0).round() / 2.0 } else { o + rnd(scale) };
            let pts = [(c(ox), c(oy)), (c(ox), c(oy)), (c(ox), c(oy))];
            let v = |p: (f32, f32)| Vertex { pos: Pos2::new(p.0, p.1), uv: Pos2::ZERO, color: Color32::WHITE };
            let (a, b, cc) = (v(pts[0]), v(pts[1]), v(pts[2]));
            let mut frame = vec![0u8; w * h * 4];
            tri(&mut frame, w, [0, 0, w as i32, h as i32], &a, &b, &cc);
            let p: Vec<[i64; 2]> = pts.iter().map(|p| [snap(p.0) as i64, snap(p.1) as i64]).collect();
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
            let sign = area.signum();
            for y in 0..h as i64 {
                for x in 0..w as i64 {
                    let (px, py) = (x * 256 + 128, y * 256 + 128);
                    let inside = area != 0
                        && (0..3).all(|i| {
                            let (q, r) = (p[(i + 1) % 3], p[(i + 2) % 3]);
                            let e = ((q[0] - px) * (r[1] - py) - (q[1] - py) * (r[0] - px)) * sign;
                            let (dx, dy) = ((q[1] - r[1]) * sign, (r[0] - q[0]) * sign);
                            e > 0 || (e == 0 && (dx > 0 || (dx == 0 && dy > 0)))
                        });
                    let drawn = frame[(y as usize * w + x as usize) * 4] != 0;
                    assert_eq!(drawn, inside, "case {case}: pixel {x},{y} of {pts:?}");
                    checked += inside as u64;
                }
            }
        }
        assert!(checked > 100_000, "{checked}");
    }

    /// Colours between the vertices and texels follow the positions: a gradient's ends and
    /// middle, and a texture's texels under nearest and bilinear sampling.
    #[test]
    fn gradients_and_textures_are_interpolated() {
        use egui::epaint::{Color32, Pos2};
        let (w, h) = (64usize, 8usize);
        let v = |x: f32, y: f32, u: f32, c: Color32| Vertex { pos: Pos2::new(x, y), uv: Pos2::new(u, 0.5), color: c };
        // Black at x = 0 to white at x = 64 over an opaque quad.
        let (k, wh) = (Color32::BLACK, Color32::WHITE);
        let q = [v(0.0, 0.0, 0.0, k), v(64.0, 0.0, 1.0, wh), v(64.0, 8.0, 1.0, wh), v(0.0, 8.0, 0.0, k)];
        let mut frame = vec![0u8; w * h * 4];
        tri(&mut frame, w, [0, 0, 64, 8], &q[0], &q[1], &q[2]);
        tri(&mut frame, w, [0, 0, 64, 8], &q[0], &q[2], &q[3]);
        for y in 0..h {
            for x in 0..w {
                let want = ((x as f32 + 0.5) / 64.0 * 255.0).round() as i32;
                let got = frame[(y * w + x) * 4] as i32;
                assert!((got - want).abs() <= 1, "gradient at {x},{y}: {got}, expected {want}");
                assert_eq!(frame[(y * w + x) * 4 + 3], 255);
            }
        }
        // A 4x1 texture stretched over the quad: nearest gives 16 pixels of each texel.
        for linear in [false, true] {
            let texels: Vec<[u8; 4]> = [0u8, 80, 160, 240].iter().map(|&g| [g, g, g, 255]).collect();
            let t = Texture { width: 4, height: 1, pixels: texels, linear };
            let q: Vec<Vertex> = q.iter().map(|q| Vertex { color: Color32::WHITE, ..*q }).collect();
            let mut frame = vec![0u8; w * h * 4];
            for [i, j, k] in [[0, 1, 2], [0, 2, 3]] {
                let f = |v: &Vertex| [snap(v.pos.x), snap(v.pos.y)];
                triangle(&mut frame, w, [0, 0, 64, 8], Some(&t), [f(&q[i]), f(&q[j]), f(&q[k])], [&q[i], &q[j], &q[k]], None);
            }
            for x in 0..w {
                let got = frame[(3 * w + x) * 4] as i32;
                let want = if linear {
                    // Texel centres at x = 8, 24, 40, 56; clamped outside them.
                    (((x as f32 + 0.5 - 8.0) / 16.0).clamp(0.0, 3.0) * 80.0).round() as i32
                } else {
                    (x as i32 / 16) * 80
                };
                assert!((got - want).abs() <= 2, "linear {linear} at {x}: {got}, expected {want}");
            }
        }
    }

    /// The rectangle fill writes exactly the pixels the two triangles write, with the same
    /// colour, for rectangles at whole, half and arbitrary coordinates, split along either
    /// diagonal, in any vertex order, inside a clip box.
    #[test]
    fn rect_fill_matches_the_two_triangles() {
        use egui::epaint::{Color32, Pos2};
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let (w, h) = (96usize, 80usize);
        let mut filled = 0u64;
        for case in 0..6000 {
            // Coordinates on a grid of 1, 1/2 or 1/8 pixel, or arbitrary.
            let mut coord = |range: u64| match case % 4 {
                0 => rnd(range) as f32,
                1 => rnd(range * 2) as f32 * 0.5,
                2 => rnd(range * 8) as f32 * 0.125,
                _ => rnd(range * 1000) as f32 / 1000.0,
            };
            let (xa, xb, ya, yb) = (coord(100) - 4.0, coord(100) - 4.0, coord(84) - 4.0, coord(84) - 4.0);
            let (x0, x1, y0, y1) = (xa.min(xb), xa.max(xb), ya.min(yb), ya.max(yb));
            let color = Color32::from_rgb(rnd(256) as u8, rnd(256) as u8, rnd(256) as u8);
            let v = |x: f32, y: f32| Vertex { pos: Pos2::new(x, y), uv: Pos2::ZERO, color };
            let corners = [v(x0, y0), v(x1, y0), v(x1, y1), v(x0, y1)];
            let mut order = match rnd(4) {
                0 => [0u32, 1, 2, 0, 2, 3],
                1 => [1, 2, 3, 1, 3, 0],
                2 => [2, 1, 0, 3, 2, 0],
                _ => [0, 2, 1, 2, 0, 3],
            };
            order[..3].rotate_left(rnd(3) as usize);
            order[3..].rotate_left(rnd(3) as usize);
            let clip = [rnd(20) as i32, rnd(20) as i32, w as i32 - rnd(20) as i32, h as i32 - rnd(20) as i32];
            let mut by_triangles = vec![7u8; w * h * 4];
            for t in order.chunks_exact(3) {
                tri(&mut by_triangles, w, clip, &corners[t[0] as usize], &corners[t[1] as usize], &corners[t[2] as usize]);
            }
            let mut by_fill = vec![7u8; w * h * 4];
            let fixed: Vec<[i32; 2]> = corners.iter().map(|c| [snap(c.pos.x), snap(c.pos.y)]).collect();
            match opaque_quad(&corners, &fixed, &order[..3], &order[3..], None) {
                Some(q) => filled += fill_rect(&mut by_fill, w, clip, &q),
                None => {
                    assert!(x0 == x1 || y0 == y1, "case {case}: a rectangle was not recognised");
                    continue;
                }
            }
            assert!(by_triangles == by_fill, "case {case}: {x0},{y0} - {x1},{y1} clip {clip:?} order {order:?}");
        }
        assert!(filled > 1_000_000, "{filled}");
    }

    /// A sequence of frames, each changing a few shapes of the previous one: painting only the
    /// changed tiles gives exactly the picture a full repaint gives, and repaints fewer tiles.
    #[test]
    fn changed_tiles_give_the_full_picture() {
        use egui::epaint::{Color32, Mesh, Pos2, Rect};
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let (w, h) = (300usize, 200usize);
        // Shapes: (is rectangle, corner points, colour).
        let shape = |rnd: &mut dyn FnMut(u64) -> u64| {
            let p = |rnd: &mut dyn FnMut(u64) -> u64| Pos2::new(rnd(2600) as f32 / 8.0 - 10.0, rnd(1800) as f32 / 8.0 - 10.0);
            let a = if rnd(3) == 0 { 255 } else { rnd(255) as u8 };
            let color = Color32::from_rgba_premultiplied((rnd(256) * a as u64 / 255) as u8, (rnd(256) * a as u64 / 255) as u8, (rnd(256) * a as u64 / 255) as u8, a);
            (rnd(2) == 0, [p(rnd), p(rnd), p(rnd)], color, rnd(4) == 0)
        };
        let mut shapes: Vec<_> = (0..120).map(|_| shape(&mut rnd)).collect();
        let build = |shapes: &[(bool, [Pos2; 3], Color32, bool)]| {
            let mut out = Vec::new();
            for chunk in shapes.chunks(20) {
                let mut mesh = Mesh::default();
                for (rect, p, color, gradient) in chunk {
                    let n = mesh.vertices.len() as u32;
                    let v = |pos: Pos2, color: Color32| Vertex { pos, uv: Pos2::ZERO, color };
                    if *rect {
                        let r = Rect::from_two_pos(p[0], p[1]);
                        for pos in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                            mesh.vertices.push(v(pos, *color));
                        }
                        mesh.indices.extend([n, n + 1, n + 2, n, n + 2, n + 3]);
                    } else {
                        let last = if *gradient { Color32::TRANSPARENT } else { *color };
                        mesh.vertices.extend([v(p[0], *color), v(p[1], *color), v(p[2], last)]);
                        mesh.indices.extend([n, n + 1, n + 2]);
                    }
                }
                let clip = Rect::from_min_max(Pos2::new(5.0, 3.0), Pos2::new(290.5, 196.0));
                out.push(ClippedPrimitive { clip_rect: clip, primitive: Primitive::Mesh(mesh) });
            }
            out
        };
        let mut partial = Renderer::new(w, h);
        let mut full = Renderer::new(w, h);
        let (mut repainted, mut tiles) = (0u64, 0u64);
        for frame in 0..60 {
            // Nothing, one shape or several change; the first frame paints everything.
            for _ in 0..[0, 1, 1, 4][frame % 4] {
                let i = rnd(shapes.len() as u64) as usize;
                shapes[i] = shape(&mut rnd);
            }
            let prims = build(&shapes);
            let c = partial.paint_changed([10, 20, 30], &prims, 1.0);
            full.clear([10, 20, 30]);
            full.paint(&prims, 1.0);
            if frame % 4 == 0 && frame > 0 {
                assert_eq!(c.repainted, 0, "frame {frame}: an unchanged frame repainted tiles");
            }
            repainted += c.repainted as u64;
            tiles += c.tiles as u64;
            assert!(partial.frame == full.frame, "frame {frame}: the pictures differ");
        }
        assert!(repainted * 4 < tiles * 3, "{repainted} of {tiles} tiles repainted");
    }

    /// Triangle pairs that are not one opaque rectangle keep the general path.
    #[test]
    fn other_pairs_are_not_taken_for_rectangles() {
        use egui::epaint::{Color32, Pos2};
        let v = |x: f32, y: f32, color: Color32| Vertex { pos: Pos2::new(x, y), uv: Pos2::ZERO, color };
        let red = Color32::RED;
        let quad = |c: [Color32; 4], p: [(f32, f32); 4]| [v(p[0].0, p[0].1, c[0]), v(p[1].0, p[1].1, c[1]), v(p[2].0, p[2].1, c[2]), v(p[3].0, p[3].1, c[3])];
        let square = [(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
        let is_quad = |vs: &[Vertex], i: [u32; 6]| {
            let fixed: Vec<[i32; 2]> = vs.iter().map(|c| [snap(c.pos.x), snap(c.pos.y)]).collect();
            opaque_quad(vs, &fixed, &i[..3], &i[3..], None).is_some()
        };
        assert!(is_quad(&quad([red; 4], square), [0, 1, 2, 0, 2, 3]));
        // Translucent, a gradient, a slanted quad, the same half twice, a triangle and a sliver.
        assert!(!is_quad(&quad([Color32::from_rgba_premultiplied(9, 0, 0, 100); 4], square), [0, 1, 2, 0, 2, 3]));
        assert!(!is_quad(&quad([red, red, Color32::BLUE, red], square), [0, 1, 2, 0, 2, 3]));
        assert!(!is_quad(&quad([red; 4], [(0.0, 0.0), (8.0, 1.0), (8.0, 8.0), (0.0, 8.0)]), [0, 1, 2, 0, 2, 3]));
        assert!(!is_quad(&quad([red; 4], square), [0, 1, 2, 2, 0, 1]));
        assert!(!is_quad(&quad([red; 4], square), [0, 1, 2, 0, 1, 1]));
    }

    /// Two triangles sharing an edge (a quad split along its diagonal, and a fan) cover every
    /// pixel of their union exactly once: translucent quads show no seam.
    #[test]
    fn shared_edges_are_drawn_once() {
        use egui::epaint::{Color32, Pos2};
        let v = |x: f32, y: f32| Vertex { pos: Pos2::new(x, y), uv: Pos2::ZERO, color: Color32::from_rgba_premultiplied(100, 0, 0, 100) };
        // A translucent colour over black: drawn once, red is 100; drawn twice, about 160.
        let cases: [[(f32, f32); 4]; 3] = [
            [(10.0, 10.0), (138.0, 10.0), (138.0, 138.0), (10.0, 138.0)],
            [(3.5, 7.25), (90.0, 2.0), (120.0, 99.0), (5.0, 80.0)],
            [(0.0, 0.0), (64.0, 0.0), (64.0, 64.0), (0.0, 64.0)],
        ];
        for quad in cases {
            for diagonal in [0usize, 1] {
                let (w, h) = (160usize, 160usize);
                let mut frame = vec![0u8; w * h * 4];
                let p: Vec<Vertex> = quad.iter().map(|&(x, y)| v(x, y)).collect();
                let tris = if diagonal == 0 { [[0, 1, 2], [0, 2, 3]] } else { [[1, 2, 3], [1, 3, 0]] };
                for [i, j, k] in tris {
                    tri(&mut frame, w, [0, 0, w as i32, h as i32], &p[i], &p[j], &p[k]);
                }
                let twice = frame.chunks(4).filter(|px| px[0] > 110).count();
                let once = frame.chunks(4).filter(|px| (90..=110).contains(&px[0])).count();
                assert_eq!(twice, 0, "quad {quad:?} diagonal {diagonal}: {twice} pixels drawn twice");
                assert!(once > 1000, "quad {quad:?}: only {once} pixels drawn");
            }
        }
    }
}

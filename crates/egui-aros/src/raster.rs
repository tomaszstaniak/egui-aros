//! A software renderer for egui's tessellated output: textured, vertex-coloured triangles with
//! premultiplied alpha, drawn into an RGBA frame buffer that the window blits. No GPU is needed
//! (AROS has no wgpu backend), and egui's own feathering does the anti-aliasing.
//!
//! Origin: `graphics/photocraft/photocraft-aros/src/raster.rs` in aros-development-team/ports
//! (commit 8a6faee1, Nick Andrews; as of 685982ed), MIT OR Apache-2.0. Changed since, in the
//! WordCraft and PdfCraft AROS ports: per-kind paint statistics, constant-UV triangles sampled
//! once, and per-row spans that test only the pixels a triangle can cover.

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
}

/// Side of the squares `paint_changed` compares and repaints, in pixels.
pub const TILE: usize = 64;

/// What [`Renderer::paint_changed`] did.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Changed {
    pub tiles: u32,
    pub repainted: u32,
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
        if self.measure || tw > u16::MAX as usize || th > u16::MAX as usize {
            self.clear(rgb);
            self.paint(primitives, pixels_per_point);
            return Changed { tiles: total, repainted: total };
        }
        const K: u64 = 0x9e37_79b9_7f4a_7c15;
        let mix = |h: u64, v: u64| (h.rotate_left(5) ^ v).wrapping_mul(K);
        let seed = mix(mix(u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 0]) as u64, pixels_per_point.to_bits() as u64), self.width as u64);
        let mut now = vec![seed; tw * th];
        let mut spans = std::mem::take(&mut self.spans);
        spans.clear();
        const NONE: [u16; 4] = [u16::MAX; 4];
        // The pixel clip box of a primitive, as `paint` computes it.
        let (fw, fh) = (self.width as i32, self.height as i32);
        let clip_box = |p: &ClippedPrimitive| {
            let c = p.clip_rect;
            [
                ((c.min.x * pixels_per_point).floor() as i32).max(0),
                ((c.min.y * pixels_per_point).floor() as i32).max(0),
                ((c.max.x * pixels_per_point).ceil() as i32).min(fw),
                ((c.max.y * pixels_per_point).ceil() as i32).min(fh),
            ]
        };
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let clip = clip_box(p);
            let empty = clip[2] <= clip[0] || clip[3] <= clip[1];
            let texture = match mesh.texture_id {
                TextureId::Managed(n) => n << 1,
                TextureId::User(n) => (n << 1) | 1,
            };
            let version = self.texture_versions.get(&mesh.texture_id).copied().unwrap_or(0);
            let base = mix(mix(mix(mix(texture, version), (clip[0] as u64) << 32 | clip[1] as u64), (clip[2] as u64) << 32 | clip[3] as u64), 1);
            for tri in mesh.indices.chunks_exact(3) {
                let (Some(a), Some(b), Some(c)) =
                    (mesh.vertices.get(tri[0] as usize), mesh.vertices.get(tri[1] as usize), mesh.vertices.get(tri[2] as usize))
                else {
                    spans.push(NONE);
                    continue;
                };
                if empty {
                    spans.push(NONE);
                    continue;
                }
                // The bounding box `triangle` rasterizes within.
                let ppp = pixels_per_point;
                let min_x = ((a.pos.x.min(b.pos.x).min(c.pos.x) * ppp).floor() as i32).max(clip[0]);
                let max_x = ((a.pos.x.max(b.pos.x).max(c.pos.x) * ppp).ceil() as i32).min(clip[2]);
                let min_y = ((a.pos.y.min(b.pos.y).min(c.pos.y) * ppp).floor() as i32).max(clip[1]);
                let max_y = ((a.pos.y.max(b.pos.y).max(c.pos.y) * ppp).ceil() as i32).min(clip[3]);
                if min_x >= max_x || min_y >= max_y {
                    spans.push(NONE);
                    continue;
                }
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
            return Changed { tiles: total, repainted: 0 };
        }
        // Per tile row, the runs of changed tiles as pixel ranges; cleared, then painted.
        let mut runs: Vec<Vec<(i32, i32)>> = vec![Vec::new(); th];
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
        let mut n = 0usize;
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let clip = clip_box(p);
            let texture = self.textures.get(&mesh.texture_id);
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
                let (a, b, c) = (&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]);
                // The other half of a rectangle has the same tiles: both are done here.
                let quad = if i < count && spans[n + i] == span {
                    opaque_quad(&mesh.vertices, tri, &mesh.indices[i * 3..i * 3 + 3], texture, pixels_per_point)
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
                            None => triangle(&mut self.frame, self.width, part, pixels_per_point, texture, a, b, c, None),
                        }
                    }
                }
            }
            n += count;
        }
        self.tiles = now;
        self.spans = spans;
        Changed { tiles: total, repainted }
    }

    pub fn paint(&mut self, primitives: &[ClippedPrimitive], pixels_per_point: f32) {
        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let clip = p.clip_rect;
            let x0 = ((clip.min.x * pixels_per_point).floor() as i32).max(0);
            let y0 = ((clip.min.y * pixels_per_point).floor() as i32).max(0);
            let x1 = ((clip.max.x * pixels_per_point).ceil() as i32).min(self.width as i32);
            let y1 = ((clip.max.y * pixels_per_point).ceil() as i32).min(self.height as i32);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
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
                    if let Some(q) = opaque_quad(&mesh.vertices, tri, next, texture, pixels_per_point) {
                        let started = measure.then(std::time::Instant::now);
                        let n = fill_rect(&mut self.frame, self.width, [x0, y0, x1, y1], &q);
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
                let (Some(a), Some(b), Some(c)) =
                    (mesh.vertices.get(tri[0] as usize), mesh.vertices.get(tri[1] as usize), mesh.vertices.get(tri[2] as usize))
                else {
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
                triangle(&mut self.frame, self.width, [x0, y0, x1, y1], pixels_per_point, texture, a, b, c, stats);
            }
            if let Some((k, t)) = run {
                self.stats.kinds[k as usize].us += t.elapsed().as_micros() as u64;
            }
        }
    }
}

/// An opaque axis-aligned rectangle in pixel coordinates and its colour.
struct Quad {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    rgba: [u8; 4],
}

/// The rectangle two consecutive triangles make, when they are its two halves, of one colour,
/// sample one texel (egui's fills) and the result is opaque. The colour is computed as
/// `triangle` computes it.
fn opaque_quad(vertices: &[Vertex], t0: &[u32], t1: &[u32], texture: Option<&Texture>, ppp: f32) -> Option<Quad> {
    let v = |i: u32| vertices.get(i as usize);
    let (a, b, c) = (v(t0[0])?, v(t0[1])?, v(t0[2])?);
    let (d, e, f) = (v(t1[0])?, v(t1[1])?, v(t1[2])?);
    let all = [a, b, c, d, e, f];
    if all.iter().any(|q| q.color != a.color || q.uv != a.uv) {
        return None;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (a.pos.x, a.pos.x, a.pos.y, a.pos.y);
    for q in all {
        x0 = x0.min(q.pos.x);
        x1 = x1.max(q.pos.x);
        y0 = y0.min(q.pos.y);
        y1 = y1.max(q.pos.y);
    }
    if !(x0 < x1 && y0 < y1) || !x0.is_finite() || !x1.is_finite() || !y0.is_finite() || !y1.is_finite() {
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
    let col = a.color.to_array();
    let color = [col[0] as f32, col[1] as f32, col[2] as f32, col[3] as f32];
    let src = match texture {
        Some(t) => {
            let s = t.sample(a.uv.x, a.uv.y);
            [color[0] * s[0] / 255.0, color[1] * s[1] / 255.0, color[2] * s[2] / 255.0, color[3] * s[3] / 255.0]
        }
        None => color,
    };
    if src[3] < 254.5 {
        return None;
    }
    Some(Quad { x0: x0 * ppp, y0: y0 * ppp, x1: x1 * ppp, y1: y1 * ppp, rgba: [src[0] as u8, src[1] as u8, src[2] as u8, 255] })
}

/// Fills the pixels whose centres lie inside the rectangle, left and top edges included, right
/// and bottom excluded (the rule `triangle` follows), within the clip box. Returns the count.
fn fill_rect(frame: &mut [u8], width: usize, clip: [i32; 4], q: &Quad) -> u64 {
    // The first pixel whose centre (n + 0.5) is at or after the edge.
    let first = |edge: f32| (edge - 0.5).ceil() as i32;
    let xa = first(q.x0).max(clip[0]);
    let xb = first(q.x1).min(clip[2]);
    let ya = first(q.y0).max(clip[1]);
    let yb = first(q.y1).min(clip[3]);
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

/// Rasterizes one triangle into `frame` within the clip box (pixel units, max exclusive).
#[allow(clippy::too_many_arguments)]
fn triangle(
    frame: &mut [u8],
    width: usize,
    clip: [i32; 4],
    ppp: f32,
    texture: Option<&Texture>,
    a: &Vertex,
    b: &Vertex,
    c: &Vertex,
    mut stats: Option<&mut KindStats>,
) {
    let (ax, ay) = (a.pos.x * ppp, a.pos.y * ppp);
    let (bx, by) = (b.pos.x * ppp, b.pos.y * ppp);
    let (cx, cy) = (c.pos.x * ppp, c.pos.y * ppp);
    let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
    if area.abs() < 1e-6 || !area.is_finite() {
        return;
    }
    let inv_area = 1.0 / area;
    let min_x = (ax.min(bx).min(cx).floor() as i32).max(clip[0]);
    let max_x = (ax.max(bx).max(cx).ceil() as i32).min(clip[2]);
    let min_y = (ay.min(by).min(cy).floor() as i32).max(clip[1]);
    let max_y = (ay.max(by).max(cy).ceil() as i32).min(clip[3]);
    if min_x >= max_x || min_y >= max_y {
        return;
    }
    if let Some(st) = stats.as_deref_mut() {
        st.triangles += 1;
        // Pixels tested are counted in the row loop (`tested`).
    }
    // Counted per triangle, added once at the end: no counter work in the pixel loop beyond
    // a few local increments.
    let (mut covered, mut sampled, mut blended, mut opaque, mut tested) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let ca = a.color.to_array();
    let cb = b.color.to_array();
    let cc = c.color.to_array();
    let flat_color = ca == cb && cb == cc;
    // A triangle whose three UVs are equal samples one point of its texture at every pixel
    // (egui's fills use the atlas's white texel): sample once. With a flat colour, the whole
    // source colour is the same everywhere too. Same arithmetic as the per-pixel path.
    let uniform = (a.uv == b.uv && b.uv == c.uv).then(|| texture.map(|t| t.sample(a.uv.x, a.uv.y)));
    let modulate =
        |color: [f32; 4], s: [f32; 4]| [color[0] * s[0] / 255.0, color[1] * s[1] / 255.0, color[2] * s[2] / 255.0, color[3] * s[3] / 255.0];
    let flat_src = match uniform {
        Some(sample) if flat_color => {
            let color = [ca[0] as f32, ca[1] as f32, ca[2] as f32, ca[3] as f32];
            Some(sample.map_or(color, |s| modulate(color, s)))
        }
        _ => None,
    };
    // How each edge function changes per pixel step to the right (d/dx of the expressions below).
    let e0_dx = by - cy;
    let e1_dx = cy - ay;
    let e2_dx = ay - by;
    // Normalized to the triangle's winding: inside when all three are >= 0.
    let sign = if area > 0.0 { 1.0 } else { -1.0 };
    // A pixel centre exactly on an edge belongs to one triangle only (the top-left rule of
    // D3D and GL rasterizers, y down): to the one whose edge is a left edge (the function grows
    // to the right) or a top edge (horizontal, growing downwards). Without it, a pixel on the
    // edge two triangles share was drawn by both, which shows as a line where they are
    // translucent (the diagonal of every image quad at integer positions).
    let top_left = |dx: f32, dy: f32| {
        let (dx, dy) = (dx * sign, dy * sign);
        dx > 0.0 || (dx == 0.0 && dy > 0.0)
    };
    // d/dy of the three edge functions.
    let (tl0, tl1, tl2) = (top_left(e0_dx, cx - bx), top_left(e1_dx, ax - cx), top_left(e2_dx, bx - ax));
    let inside = |w: f32, tl: bool| {
        let w = w * sign;
        w > 0.0 || (w == 0.0 && tl)
    };
    for py in min_y..max_y {
        let sy = py as f32 + 0.5;
        let sx0 = min_x as f32 + 0.5;
        let mut w0 = (bx - sx0) * (cy - sy) - (by - sy) * (cx - sx0);
        let mut w1 = (cx - sx0) * (ay - sy) - (cy - sy) * (ax - sx0);
        let mut w2 = (ax - sx0) * (by - sy) - (ay - sy) * (bx - sx0);
        let row = py as usize * width;
        // The pixels of this row that can be inside: each edge function is linear along the
        // row, so its sign changes at most once. Widened by a pixel on each side; the exact
        // test below still decides every pixel, so coverage follows the same rule.
        let Some((start, end)) = row_span([(w0, e0_dx), (w1, e1_dx), (w2, e2_dx)], sign, max_x - min_x) else { continue };
        w0 += e0_dx * start as f32;
        w1 += e1_dx * start as f32;
        w2 += e2_dx * start as f32;
        tested += (end - start) as u64;
        for px in min_x + start..min_x + end {
            if inside(w0, tl0) && inside(w1, tl1) && inside(w2, tl2) {
                covered += 1;
                let src = match flat_src {
                    Some(src) => src,
                    None => {
                        let l0 = w0 * inv_area;
                        let l1 = w1 * inv_area;
                        let l2 = w2 * inv_area;
                        let color = if flat_color {
                            [ca[0] as f32, ca[1] as f32, ca[2] as f32, ca[3] as f32]
                        } else {
                            let mut v = [0.0f32; 4];
                            for i in 0..4 {
                                v[i] = ca[i] as f32 * l0 + cb[i] as f32 * l1 + cc[i] as f32 * l2;
                            }
                            v
                        };
                        match (uniform, texture) {
                            (Some(Some(s)), _) => modulate(color, s),
                            (_, Some(t)) => {
                                let u = a.uv.x * l0 + b.uv.x * l1 + c.uv.x * l2;
                                let v = a.uv.y * l0 + b.uv.y * l1 + c.uv.y * l2;
                                sampled += 1;
                                modulate(color, t.sample(u, v))
                            }
                            (_, None) => color,
                        }
                    }
                };
                let alpha = src[3];
                if alpha > 0.0 {
                    let i = (row + px as usize) * 4;
                    if let Some(dst) = frame.get_mut(i..i + 4) {
                        if alpha >= 254.5 {
                            opaque += 1;
                            dst[0] = src[0] as u8;
                            dst[1] = src[1] as u8;
                            dst[2] = src[2] as u8;
                            dst[3] = 255;
                        } else {
                            blended += 1;
                            let keep = (255.0 - alpha) / 255.0;
                            dst[0] = (src[0] + dst[0] as f32 * keep).min(255.0) as u8;
                            dst[1] = (src[1] + dst[1] as f32 * keep).min(255.0) as u8;
                            dst[2] = (src[2] + dst[2] as f32 * keep).min(255.0) as u8;
                            dst[3] = (alpha + dst[3] as f32 * keep).min(255.0) as u8;
                        }
                    }
                }
            }
            w0 += e0_dx;
            w1 += e1_dx;
            w2 += e2_dx;
        }
    }
    if let Some(st) = stats {
        st.visited += tested;
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

/// Of the `len` pixels of a row, the range `start..end` (relative to the row's first pixel)
/// outside of which some edge function is certainly negative: `edges` holds each function's
/// value at the first pixel and its step per pixel, `sign` the triangle's winding. `None`
/// when no pixel of the row can be inside.
fn row_span(edges: [(f32, f32); 3], sign: f32, len: i32) -> Option<(i32, i32)> {
    let (mut lo, mut hi) = (0.0f32, len as f32);
    for (w, step) in edges {
        let (w, step) = (w * sign, step * sign);
        if step > 0.0 {
            lo = lo.max(-w / step);
        } else if step < 0.0 {
            hi = hi.min(-w / step);
        } else if w < 0.0 {
            return None;
        }
    }
    if !(lo <= hi) {
        return None;
    }
    let start = ((lo.floor() as i32) - 1).clamp(0, len);
    let end = ((hi.ceil() as i32) + 2).clamp(0, len);
    (start < end).then_some((start, end))
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

    /// Every pixel the full test of the old loop finds inside (edge functions accumulated from
    /// the bounding box's left edge) lies within the row's span, for many random triangles.
    #[test]
    fn row_span_keeps_every_covered_pixel() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move |range: f32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 1_000_000) as f32 / 1_000_000.0 * range
        };
        let mut checked = 0u64;
        for case in 0..20_000 {
            // Large, small, thin and sub-pixel triangles.
            let scale = [400.0, 40.0, 4.0, 1.5][case % 4];
            let (ox, oy) = (rnd(300.0), rnd(300.0));
            let (ax, ay, bx, by, cx, cy) = (ox + rnd(scale), oy + rnd(scale), ox + rnd(scale), oy + rnd(scale), ox + rnd(scale), oy + rnd(scale));
            let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
            if area.abs() < 1e-6 {
                continue;
            }
            let sign = if area > 0.0 { 1.0 } else { -1.0 };
            let min_x = ax.min(bx).min(cx).floor() as i32;
            let max_x = ax.max(bx).max(cx).ceil() as i32;
            let min_y = ay.min(by).min(cy).floor() as i32;
            let max_y = ay.max(by).max(cy).ceil() as i32;
            let (e0, e1, e2) = (by - cy, cy - ay, ay - by);
            for py in min_y..max_y {
                let sy = py as f32 + 0.5;
                let sx0 = min_x as f32 + 0.5;
                let mut w0 = (bx - sx0) * (cy - sy) - (by - sy) * (cx - sx0);
                let mut w1 = (cx - sx0) * (ay - sy) - (cy - sy) * (ax - sx0);
                let mut w2 = (ax - sx0) * (by - sy) - (ay - sy) * (bx - sx0);
                let span = row_span([(w0, e0), (w1, e1), (w2, e2)], sign, max_x - min_x);
                for k in 0..max_x - min_x {
                    if w0 * sign >= 0.0 && w1 * sign >= 0.0 && w2 * sign >= 0.0 {
                        let (s, e) = span.unwrap_or_else(|| panic!("case {case} row {py}: covered pixel {k} but no span"));
                        assert!(s <= k && k < e, "case {case} row {py}: covered pixel {k} outside {s}..{e}");
                        checked += 1;
                    }
                    w0 += e0;
                    w1 += e1;
                    w2 += e2;
                }
            }
        }
        assert!(checked > 100_000, "{checked}");
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
                // Never exactly on a pixel centre: there the triangles' accumulated rounding
                // decides, while the fill applies the rule exactly (the grids above are exact).
                _ => rnd(range * 1000) as f32 / 1000.0 + 0.0003,
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
                triangle(&mut by_triangles, w, clip, 1.0, None, &corners[t[0] as usize], &corners[t[1] as usize], &corners[t[2] as usize], None);
            }
            let mut by_fill = vec![7u8; w * h * 4];
            match opaque_quad(&corners, &order[..3], &order[3..], None, 1.0) {
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
    /// changed tiles gives the picture a full repaint gives, and repaints fewer tiles. The two
    /// may differ by rounding where a triangle is cut at a tile edge (its edge functions are
    /// accumulated from another start), so single levels of a channel are allowed.
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
        let (mut repainted, mut tiles, mut off_by_one) = (0u64, 0u64, 0u64);
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
            for (i, (a, b)) in partial.frame.iter().zip(&full.frame).enumerate() {
                let d = a.abs_diff(*b);
                assert!(d <= 1, "frame {frame}: pixel {} differs by {d}", i / 4);
                off_by_one += d as u64;
            }
        }
        assert!(repainted * 4 < tiles * 3, "{repainted} of {tiles} tiles repainted");
        assert!(off_by_one < 200, "{off_by_one} channel values differ by one level");
    }

    /// Triangle pairs that are not one opaque rectangle keep the general path.
    #[test]
    fn other_pairs_are_not_taken_for_rectangles() {
        use egui::epaint::{Color32, Pos2};
        let v = |x: f32, y: f32, color: Color32| Vertex { pos: Pos2::new(x, y), uv: Pos2::ZERO, color };
        let red = Color32::RED;
        let quad = |c: [Color32; 4], p: [(f32, f32); 4]| [v(p[0].0, p[0].1, c[0]), v(p[1].0, p[1].1, c[1]), v(p[2].0, p[2].1, c[2]), v(p[3].0, p[3].1, c[3])];
        let square = [(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
        let is_quad = |vs: &[Vertex], i: [u32; 6]| opaque_quad(vs, &i[..3], &i[3..], None, 1.0).is_some();
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
                    triangle(&mut frame, w, [0, 0, w as i32, h as i32], 1.0, None, &p[i], &p[j], &p[k], None);
                }
                let twice = frame.chunks(4).filter(|px| px[0] > 110).count();
                let once = frame.chunks(4).filter(|px| (90..=110).contains(&px[0])).count();
                assert_eq!(twice, 0, "quad {quad:?} diagonal {diagonal}: {twice} pixels drawn twice");
                assert!(once > 1000, "quad {quad:?}: only {once} pixels drawn");
            }
        }
    }
}

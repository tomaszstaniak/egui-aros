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
}

impl Renderer {
    pub fn new(width: usize, height: usize) -> Self {
        let mut r = Self { textures: HashMap::new(), stats: PaintStats::default(), measure: false, frame: Vec::new(), width: 0, height: 0 };
        r.resize(width, height);
        r
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.frame = vec![0; width * height * 4];
        }
    }

    pub fn stride(&self) -> usize {
        self.width * 4
    }

    pub fn clear(&mut self, rgb: [u8; 3]) {
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
            for tri in mesh.indices.chunks_exact(3) {
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

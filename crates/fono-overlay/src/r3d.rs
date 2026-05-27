// SPDX-License-Identifier: GPL-3.0-only
//! Tiny CPU 3D primitives for the overlay's three-dimensional
//! visualisation styles (Lissajous wire, spectrogram terrain,
//! audio-reactive blob).
//!
//! ## Scope
//!
//! This module deliberately reimplements a sliver of a 3D math
//! library rather than pulling in `glam` / `nalgebra`. We need:
//!
//! - A `Vec3` and a column-major `Mat4` with the operations the
//!   three viz styles actually use (translate, rotate-Y, look-at,
//!   perspective, multiply).
//! - A `project` helper that takes a world-space point through the
//!   view-projection matrix and lands it in panel-pixel coordinates,
//!   returning `None` for anything behind the near plane.
//! - A `draw_line_3d` / `draw_polyline_3d` pair that delegate to
//!   the existing 2D Bresenham line primitive in
//!   [`crate::renderer::draw_line_segment`].
//! - A `DepthBuffer` carrying per-pixel z values for the styles
//!   that need correct occlusion (terrain, blob).
//! - A `draw_triangle_3d_filled` barycentric-fill primitive that
//!   honours the depth buffer.
//!
//! The math is column-major. `Mat4::mul_vec` treats the input as
//! a column vector with implicit `w = 1` and returns
//! `(x_clip, y_clip, z_clip, w_clip)` so the projection step can
//! do its own divide.
//!
//! ## Why no SIMD, no `f32x4`?
//!
//! The full per-frame load for the heaviest style (terrain) is
//! ~1920 vertex transforms and ~3700 triangles. On a Kaby Lake CPU
//! that's already comfortably under 4 ms at scalar floats. SIMD
//! would obscure the code with no observable win at 30 fps.

#![allow(
    clippy::suboptimal_flops,
    clippy::many_single_char_names,
    clippy::should_implement_trait,
    clippy::too_many_arguments
)]

use crate::renderer::{blend, draw_line_segment};

/// Right-handed coordinate system, +X right, +Y up, +Z toward
/// the viewer (so a camera at `(0, 0, -d)` looking at the origin
/// sees positive-Z faces).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0, z: 0.0 };

    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }

    #[must_use]
    pub fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }

    #[must_use]
    pub fn scale(self, k: f32) -> Self {
        Self::new(self.x * k, self.y * k, self.z * k)
    }

    #[must_use]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    #[must_use]
    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    #[must_use]
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    #[must_use]
    pub fn normalize(self) -> Self {
        let l = self.length();
        if l < 1e-6 {
            Self::ZERO
        } else {
            self.scale(1.0 / l)
        }
    }
}

/// Column-major 4×4 matrix. Indices are `m[col * 4 + row]` so
/// `Mat4::translation(tx, ty, tz)` puts the translation in the
/// final column (`m[12], m[13], m[14]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    #[must_use]
    pub const fn identity() -> Self {
        let mut m = [0.0; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        Self(m)
    }

    #[must_use]
    pub const fn translation(tx: f32, ty: f32, tz: f32) -> Self {
        let mut m = Self::identity().0;
        m[12] = tx;
        m[13] = ty;
        m[14] = tz;
        Self(m)
    }

    /// Rotation around the Y axis by `radians`.
    #[must_use]
    pub fn rotation_y(radians: f32) -> Self {
        let c = radians.cos();
        let s = radians.sin();
        let mut m = Self::identity().0;
        m[0] = c;
        m[2] = -s;
        m[8] = s;
        m[10] = c;
        Self(m)
    }

    /// Rotation around the X axis by `radians`.
    #[must_use]
    pub fn rotation_x(radians: f32) -> Self {
        let c = radians.cos();
        let s = radians.sin();
        let mut m = Self::identity().0;
        m[5] = c;
        m[6] = s;
        m[9] = -s;
        m[10] = c;
        Self(m)
    }

    /// Right-handed look-at: camera at `eye`, target at `target`,
    /// `up` is the world up axis. Builds the view matrix that maps
    /// world coordinates into the camera's local frame with the
    /// camera at the origin looking down -Z.
    #[must_use]
    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Self {
        let f = target.sub(eye).normalize();
        let s = f.cross(up).normalize();
        let u = s.cross(f);
        let mut m = [0.0; 16];
        m[0] = s.x;
        m[4] = s.y;
        m[8] = s.z;
        m[1] = u.x;
        m[5] = u.y;
        m[9] = u.z;
        m[2] = -f.x;
        m[6] = -f.y;
        m[10] = -f.z;
        m[12] = -s.dot(eye);
        m[13] = -u.dot(eye);
        m[14] = f.dot(eye);
        m[15] = 1.0;
        Self(m)
    }

    /// Right-handed perspective projection mapping the camera-
    /// space frustum (`fovy_radians`, `aspect = width / height`,
    /// near, far) into clip space.
    #[must_use]
    pub fn perspective(fovy_radians: f32, aspect: f32, near: f32, far: f32) -> Self {
        let f = 1.0 / (fovy_radians * 0.5).tan();
        let nf = 1.0 / (near - far);
        let mut m = [0.0; 16];
        m[0] = f / aspect;
        m[5] = f;
        m[10] = (far + near) * nf;
        m[11] = -1.0;
        m[14] = 2.0 * far * near * nf;
        Self(m)
    }

    /// Standard column-major matrix multiply: `self * rhs`.
    #[must_use]
    pub fn mul(self, rhs: Self) -> Self {
        let a = self.0;
        let b = rhs.0;
        let mut out = [0.0; 16];
        for c in 0..4 {
            for r in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += a[k * 4 + r] * b[c * 4 + k];
                }
                out[c * 4 + r] = sum;
            }
        }
        Self(out)
    }

    /// Transform a homogeneous point. Returns `(x, y, z, w)` in
    /// clip space; the caller is responsible for the divide.
    #[must_use]
    pub fn mul_vec(&self, v: Vec3) -> (f32, f32, f32, f32) {
        let m = self.0;
        let x = m[0] * v.x + m[4] * v.y + m[8] * v.z + m[12];
        let y = m[1] * v.x + m[5] * v.y + m[9] * v.z + m[13];
        let z = m[2] * v.x + m[6] * v.y + m[10] * v.z + m[14];
        let w = m[3] * v.x + m[7] * v.y + m[11] * v.z + m[15];
        (x, y, z, w)
    }
}

/// Pixel-space viewport `(x, y, width, height)`. The Y axis grows
/// downward (matches the renderer's framebuffer convention), so
/// `project` flips the NDC Y before scaling.
pub type Viewport = (f32, f32, f32, f32);

/// Project a world-space point through `view_proj` and map into the
/// pixel viewport. Returns `(x_px, y_px, depth)` where `depth` is in
/// `[0, 1]` with 0 = near plane, 1 = far plane. Points with
/// non-positive `w` (behind the near plane) yield `None`.
#[must_use]
pub fn project(p: Vec3, view_proj: &Mat4, vp: Viewport) -> Option<(f32, f32, f32)> {
    let (x, y, z, w) = view_proj.mul_vec(p);
    if w <= 1e-4 {
        return None;
    }
    let ndc_x = x / w;
    let ndc_y = y / w;
    let ndc_z = z / w;
    let (vx, vy, vw, vh) = vp;
    let px = vx + (ndc_x * 0.5 + 0.5) * vw;
    // Flip Y so NDC +Y (up) lands at smaller pixel-Y.
    let py = vy + (1.0 - (ndc_y * 0.5 + 0.5)) * vh;
    let depth = (ndc_z * 0.5 + 0.5).clamp(0.0, 1.0);
    Some((px, py, depth))
}

/// Project a line segment and rasterise it through the renderer's
/// 2D Bresenham line. Both endpoints must be in front of the near
/// plane for the line to draw; partial clipping is intentionally
/// not implemented (the visualisation styles never get close enough
/// to the camera for this to matter — the camera sits a few units
/// away from origin-anchored geometry).
pub fn draw_line_3d(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    a: Vec3,
    b: Vec3,
    color: u32,
    coverage_alpha: u8,
    view_proj: &Mat4,
    vp: Viewport,
) {
    let Some((ax, ay, _)) = project(a, view_proj, vp) else {
        return;
    };
    let Some((bx, by, _)) = project(b, view_proj, vp) else {
        return;
    };
    draw_line_segment(buf, stride, h, ax, ay, bx, by, color, coverage_alpha);
}

/// Draw a 3D polyline. With `fade_tail = true`, the alpha of each
/// segment ramps linearly from `1.0` at index 0 to `0.0` at the
/// last index — used by the Lissajous trail.
pub fn draw_polyline_3d(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    pts: &[Vec3],
    color: u32,
    view_proj: &Mat4,
    vp: Viewport,
    fade_tail: bool,
) {
    if pts.len() < 2 {
        return;
    }
    let n = pts.len() as f32;
    for i in 0..pts.len() - 1 {
        let alpha = if fade_tail {
            let t = i as f32 / n;
            (255.0 * (1.0 - t)) as u8
        } else {
            255
        };
        draw_line_3d(buf, stride, h, pts[i], pts[i + 1], color, alpha, view_proj, vp);
    }
}

/// Per-pixel depth buffer sized to the panel's visualisation area.
/// Cheap allocation; the renderer reuses one buffer across frames
/// and only reallocates on panel resize.
#[derive(Debug, Default)]
pub struct DepthBuffer {
    buf: Vec<f32>,
    width: u32,
    height: u32,
}

impl DepthBuffer {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self { buf: vec![1.0; (width * height) as usize], width, height }
    }

    /// Resize and clear in one step. No-op when dimensions match
    /// and `force_clear` is false.
    pub fn reset(&mut self, width: u32, height: u32) {
        if self.width != width || self.height != height {
            self.buf = vec![1.0; (width * height) as usize];
            self.width = width;
            self.height = height;
        } else {
            for v in &mut self.buf {
                *v = 1.0;
            }
        }
    }

    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        (y * self.width + x) as usize
    }

    /// True if `depth` is closer than what is already at `(x, y)`.
    /// Writes the new depth and returns true on hit; leaves the
    /// buffer untouched and returns false on occlude.
    #[inline]
    pub fn test_and_set(&mut self, x: u32, y: u32, depth: f32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let i = self.idx(x, y);
        // Safety: bounds-checked above.
        let slot = &mut self.buf[i];
        if depth < *slot {
            *slot = depth;
            true
        } else {
            false
        }
    }
}

/// Fill a 3D triangle with `color`, honouring `depth`. Pixels are
/// written through [`blend`] so non-opaque colours composite
/// correctly. Vertices are projected first; degenerate cases
/// (single point off-screen, zero area) are silently skipped.
#[allow(clippy::too_many_arguments)]
pub fn draw_triangle_3d_filled(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    a: Vec3,
    b: Vec3,
    c: Vec3,
    color: u32,
    view_proj: &Mat4,
    vp: Viewport,
    depth: &mut DepthBuffer,
) {
    let Some((ax, ay, az)) = project(a, view_proj, vp) else {
        return;
    };
    let Some((bx, by, bz)) = project(b, view_proj, vp) else {
        return;
    };
    let Some((cx, cy, cz)) = project(c, view_proj, vp) else {
        return;
    };
    // Bounding box (clipped to viewport).
    let min_x = ax.min(bx).min(cx).floor().max(0.0) as i32;
    let max_x = ax.max(bx).max(cx).ceil().min(stride as f32 - 1.0) as i32;
    let min_y = ay.min(by).min(cy).floor().max(0.0) as i32;
    let max_y = ay.max(by).max(cy).ceil().min(h as f32 - 1.0) as i32;
    if max_x <= min_x || max_y <= min_y {
        return;
    }
    // Edge-function denominator (2 × signed triangle area).
    let denom = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
    if denom.abs() < 1e-4 {
        return;
    }
    let inv_denom = 1.0 / denom;
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let pxf = px as f32 + 0.5;
            let pyf = py as f32 + 0.5;
            // Barycentric weights.
            let w0 = ((bx - pxf) * (cy - pyf) - (by - pyf) * (cx - pxf)) * inv_denom;
            let w1 = ((cx - pxf) * (ay - pyf) - (cy - pyf) * (ax - pxf)) * inv_denom;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let pdepth = w0 * az + w1 * bz + w2 * cz;
            if !depth.test_and_set(px as u32, py as u32, pdepth) {
                continue;
            }
            let idx = (py as u32 * stride + px as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                let alpha = ((color >> 24) & 0xFF) as u8;
                *slot = blend(*slot, color, alpha);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mat4_identity_roundtrip() {
        let i = Mat4::identity();
        let v = Vec3::new(1.0, 2.0, 3.0);
        let (x, y, z, w) = i.mul_vec(v);
        assert!((x - 1.0).abs() < 1e-6);
        assert!((y - 2.0).abs() < 1e-6);
        assert!((z - 3.0).abs() < 1e-6);
        assert!((w - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mat4_translate_then_identity() {
        let t = Mat4::translation(10.0, -5.0, 2.0);
        let v = Vec3::new(1.0, 1.0, 1.0);
        let (x, y, z, w) = t.mul_vec(v);
        assert!((x - 11.0).abs() < 1e-6);
        assert!((y - -4.0).abs() < 1e-6);
        assert!((z - 3.0).abs() < 1e-6);
        assert!((w - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rotation_y_quarter_turn_sends_x_to_negative_z() {
        let r = Mat4::rotation_y(std::f32::consts::FRAC_PI_2);
        let (x, _, z, _) = r.mul_vec(Vec3::new(1.0, 0.0, 0.0));
        assert!(x.abs() < 1e-5, "x close to 0: {x}");
        assert!((z - -1.0).abs() < 1e-5, "z close to -1: {z}");
    }

    #[test]
    fn matmul_associative_with_identity() {
        let m = Mat4::translation(2.0, 3.0, 4.0).mul(Mat4::rotation_y(0.7));
        let mi = m.mul(Mat4::identity());
        for i in 0..16 {
            assert!((m.0[i] - mi.0[i]).abs() < 1e-5);
        }
    }

    #[test]
    fn perspective_clips_behind_near() {
        // Build a simple view-projection with camera at (0,0,-2)
        // looking at origin. A point at z = 5 (well behind the
        // camera, since +Z is toward viewer in our look_at) ends up
        // with w <= 0 → project returns None.
        let view = Mat4::look_at(Vec3::new(0.0, 0.0, -2.0), Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective(60.0_f32.to_radians(), 4.0, 0.1, 100.0);
        let vp = proj.mul(view);
        // Point behind the camera (further -Z than the camera).
        let behind = Vec3::new(0.0, 0.0, -3.0);
        assert!(project(behind, &vp, (0.0, 0.0, 640.0, 100.0)).is_none());
    }

    #[test]
    fn project_origin_lands_at_viewport_centre() {
        let view = Mat4::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective(60.0_f32.to_radians(), 4.0, 0.1, 100.0);
        let vp = proj.mul(view);
        let r = project(Vec3::ZERO, &vp, (0.0, 0.0, 640.0, 100.0)).expect("projects");
        assert!((r.0 - 320.0).abs() < 0.5, "x near centre: {}", r.0);
        assert!((r.1 - 50.0).abs() < 0.5, "y near centre: {}", r.1);
    }

    #[test]
    fn polyline_renders_inside_panel_without_panic() {
        // Smoke test: a unit cube's edges drawn into a small
        // framebuffer must not panic and must touch at least one
        // pixel that isn't the initial fill value.
        const W: u32 = 64;
        const H: u32 = 32;
        let mut buf = vec![0u32; (W * H) as usize];
        let view = Mat4::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective(60.0_f32.to_radians(), W as f32 / H as f32, 0.1, 100.0);
        let vp = proj.mul(view);
        let viewport = (0.0, 0.0, W as f32, H as f32);
        let cube = [
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(-0.5, -0.5, -0.5),
        ];
        draw_polyline_3d(&mut buf, W, H, &cube, 0xFFFF_FFFF, &vp, viewport, false);
        let painted = buf.iter().filter(|&&p| p != 0).count();
        assert!(painted > 0, "polyline should paint at least one pixel");
    }

    #[test]
    fn depth_buffer_resets_to_far_plane() {
        let mut db = DepthBuffer::new(4, 4);
        assert!(db.test_and_set(1, 1, 0.5));
        assert!(!db.test_and_set(1, 1, 0.7));
        db.reset(4, 4);
        // After reset, 0.7 should now be the closest (passes).
        assert!(db.test_and_set(1, 1, 0.7));
    }

    #[test]
    fn depth_buffer_oob_is_safe() {
        let mut db = DepthBuffer::new(4, 4);
        assert!(!db.test_and_set(10, 10, 0.1));
        assert!(!db.test_and_set(4, 0, 0.1));
        assert!(!db.test_and_set(0, 4, 0.1));
    }

    #[test]
    fn triangle_fills_pixels_inside_panel() {
        const W: u32 = 32;
        const H: u32 = 32;
        let mut buf = vec![0u32; (W * H) as usize];
        let mut depth = DepthBuffer::new(W, H);
        let view = Mat4::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective(60.0_f32.to_radians(), 1.0, 0.1, 100.0);
        let vp = proj.mul(view);
        let viewport = (0.0, 0.0, W as f32, H as f32);
        draw_triangle_3d_filled(
            &mut buf,
            W,
            H,
            Vec3::new(-0.5, -0.5, 0.0),
            Vec3::new(0.5, -0.5, 0.0),
            Vec3::new(0.0, 0.5, 0.0),
            0xFFFF_FFFF,
            &vp,
            viewport,
            &mut depth,
        );
        let painted = buf.iter().filter(|&&p| p != 0).count();
        assert!(painted > 20, "triangle should fill many pixels, got {painted}");
    }
}

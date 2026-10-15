//! Equirectangular (latitude/longitude) projection maths for View › Equirectangular Live
//! Projection: a pinhole camera (yaw, pitch, vertical field of view) looking out from the centre
//! of the sphere a 2:1 latlong image is wrapped on.
//!
//! Conventions: world +Y is up, the camera at yaw 0 / pitch 0 looks down +Z, which is the centre
//! column of the image (longitude 0). Longitude grows to the right (+X), so the image's left edge
//! is −180° and its right edge +180°; the top row is the north pole (+90° latitude).
//! Image coordinates are in pixels with (0, 0) the top-left corner of the top-left pixel.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

/// The live projection camera of one document view. Angles in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Look direction around the vertical axis: 0 = image centre, wrapped to (−180, 180].
    pub yaw: f64,
    /// Look direction up (+) or down (−), clamped to ±[`MAX_PITCH`].
    pub pitch: f64,
    /// Vertical field of view, [`MIN_FOV`]..=[`MAX_FOV`].
    pub fov: f64,
}

pub const MAX_PITCH: f64 = 89.9;
pub const MIN_FOV: f64 = 5.0;
pub const MAX_FOV: f64 = 150.0;

impl Default for Camera {
    fn default() -> Self {
        Camera { yaw: 0.0, pitch: 0.0, fov: 90.0 }
    }
}

/// `a` wrapped to (−180, 180].
pub fn wrap_deg(a: f64) -> f64 {
    if !a.is_finite() {
        return 0.0;
    }
    let r = (a + 180.0).rem_euclid(360.0) - 180.0;
    if r == -180.0 { 180.0 } else { r }
}

impl Camera {
    /// The camera with yaw wrapped, pitch and FOV clamped (non-finite values reset).
    pub fn normalized(self) -> Camera {
        let f = |v: f64, d: f64| if v.is_finite() { v } else { d };
        Camera {
            yaw: wrap_deg(self.yaw),
            pitch: f(self.pitch, 0.0).clamp(-MAX_PITCH, MAX_PITCH),
            fov: f(self.fov, 90.0).clamp(MIN_FOV, MAX_FOV),
        }
    }

    /// Forward, right and up unit vectors.
    pub fn basis(&self) -> [[f64; 3]; 3] {
        let (sy, cy) = self.yaw.to_radians().sin_cos();
        let (sp, cp) = self.pitch.to_radians().sin_cos();
        let f = [cp * sy, sp, cp * cy];
        let r = [cy, 0.0, -sy];
        let u = [-sp * sy, cp, -sp * cy];
        [f, r, u]
    }
}

/// A camera looking through a `w` × `h` pixel view (the view's centre is the optical axis).
#[derive(Clone, Copy, Debug)]
pub struct ViewProjection {
    pub cam: Camera,
    pub w: f64,
    pub h: f64,
    basis: [[f64; 3]; 3],
    /// tan(fov / 2) / (h / 2): view-plane units per view pixel.
    k: f64,
}

impl ViewProjection {
    pub fn new(cam: Camera, w: f64, h: f64) -> Self {
        let cam = cam.normalized();
        let (w, h) = (w.max(1.0), h.max(1.0));
        let k = (cam.fov.to_radians() / 2.0).tan() / (h / 2.0);
        ViewProjection { cam, w, h, basis: cam.basis(), k }
    }

    /// The unit world direction through view pixel position (x, y).
    pub fn view_to_dir(&self, x: f64, y: f64) -> [f64; 3] {
        let [f, r, u] = self.basis;
        let dx = (x - self.w / 2.0) * self.k;
        let dy = -(y - self.h / 2.0) * self.k;
        normalize([f[0] + dx * r[0] + dy * u[0], f[1] + dx * r[1] + dy * u[1], f[2] + dx * r[2] + dy * u[2]])
    }

    /// The view pixel position world direction `d` projects to, `None` behind the camera.
    pub fn dir_to_view(&self, d: [f64; 3]) -> Option<[f64; 2]> {
        let [f, r, u] = self.basis;
        let z = dot(d, f);
        if z <= 1e-6 {
            return None;
        }
        let (x, y) = (dot(d, r) / z, dot(d, u) / z);
        Some([self.w / 2.0 + x / self.k, self.h / 2.0 - y / self.k])
    }

    /// View pixel position (x, y) → equirect pixel position in a `iw` × `ih` image.
    pub fn view_to_equirect(&self, x: f64, y: f64, iw: f64, ih: f64) -> [f64; 2] {
        dir_to_equirect(self.view_to_dir(x, y), iw, ih)
    }

    /// Equirect pixel position → view pixel position (`None` behind the camera).
    pub fn equirect_to_view(&self, u: f64, v: f64, iw: f64, ih: f64) -> Option<[f64; 2]> {
        self.dir_to_view(equirect_to_dir(u, v, iw, ih))
    }
}

impl ViewProjection {
    /// This view with its camera turned by rotation `q` (row-major 3×3): view pixel `p` looks
    /// along `q · view_to_dir(p)`. The Clone Stamp samples through it, so the source shows
    /// undistorted where the stroke is.
    pub fn rotated(&self, q: &Rot) -> ViewProjection {
        let [f, r, u] = self.basis;
        ViewProjection { basis: [rot(q, f), rot(q, r), rot(q, u)], ..*self }
    }
}

/// A rotation of the sphere, row-major.
pub type Rot = [[f64; 3]; 3];

pub const IDENTITY: Rot = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// `q · v`.
pub fn rot(q: &Rot, v: [f64; 3]) -> [f64; 3] {
    [dot(q[0], v), dot(q[1], v), dot(q[2], v)]
}

/// The smallest rotation taking unit direction `a` to unit direction `b` (Rodrigues). Opposite
/// directions turn half way round an axis perpendicular to `a`.
pub fn rotation_between(a: [f64; 3], b: [f64; 3]) -> Rot {
    let (a, b) = (normalize(a), normalize(b));
    let c = dot(a, b);
    let mut k = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let mut s = dot(k, k).sqrt();
    if s < 1e-12 {
        if c > 0.0 {
            return IDENTITY;
        }
        // 180°: any perpendicular axis.
        let t = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        k = normalize([a[1] * t[2] - a[2] * t[1], a[2] * t[0] - a[0] * t[2], a[0] * t[1] - a[1] * t[0]]);
        s = 0.0;
    } else {
        k = [k[0] / s, k[1] / s, k[2] / s];
    }
    let v = 1.0 - c;
    let [x, y, z] = k;
    [
        [c + x * x * v, x * y * v - z * s, x * z * v + y * s],
        [y * x * v + z * s, c + y * y * v, y * z * v - x * s],
        [z * x * v - y * s, z * y * v + x * s, c + z * z * v],
    ]
}

/// A rotation from `"rotation"`'s 9 numbers (row-major); `None` unless it is finite and close to
/// orthonormal.
pub fn rot_from_slice(v: &[f64]) -> Option<Rot> {
    if v.len() != 9 || !v.iter().all(|x| x.is_finite()) {
        return None;
    }
    let q = [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    for i in 0..3 {
        for j in 0..3 {
            let d = dot(q[i], q[j]) - if i == j { 1.0 } else { 0.0 };
            if d.abs() > 1e-3 {
                return None;
            }
        }
    }
    Some(q)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let n = dot(a, a).sqrt();
    if n > 0.0 { [a[0] / n, a[1] / n, a[2] / n] } else { [0.0, 0.0, 1.0] }
}

/// Unit direction → equirect pixel position; `u` in [0, iw), `v` in [0, ih].
pub fn dir_to_equirect(d: [f64; 3], iw: f64, ih: f64) -> [f64; 2] {
    let lon = d[0].atan2(d[2]);
    let lat = d[1].clamp(-1.0, 1.0).asin();
    let u = ((lon / TAU + 0.5) * iw).rem_euclid(iw);
    let v = (0.5 - lat / PI) * ih;
    [u, v]
}

/// Equirect pixel position → unit direction. `u` wraps; `v` is clamped to the poles.
pub fn equirect_to_dir(u: f64, v: f64, iw: f64, ih: f64) -> [f64; 3] {
    let lon = (u / iw - 0.5) * TAU;
    let lat = ((0.5 - v / ih) * PI).clamp(-FRAC_PI_2, FRAC_PI_2);
    let (sl, cl) = lat.sin_cos();
    [cl * lon.sin(), sl, cl * lon.cos()]
}

/// The equirect pixel columns and rows a view-space rectangle `[x0, y0, x1, y1]` can touch, as
/// one or two half-open pixel rects `[u0, v0, u1, v1]` (two when it straddles the ±180° seam,
/// the full width when it contains a pole), padded by `pad` pixels.
pub fn view_rect_to_equirect(p: &ViewProjection, r: [f64; 4], iw: u32, ih: u32, pad: f64) -> Vec<[i64; 4]> {
    let (fw, fh) = (iw as f64, ih as f64);
    let n = 24;
    let mut us = Vec::with_capacity((n + 1) * (n + 1));
    let (mut vmin, mut vmax) = (f64::MAX, f64::MIN);
    for j in 0..=n {
        for i in 0..=n {
            let x = r[0] + (r[2] - r[0]) * i as f64 / n as f64;
            let y = r[1] + (r[3] - r[1]) * j as f64 / n as f64;
            let [u, v] = p.view_to_equirect(x, y, fw, fh);
            us.push(u);
            vmin = vmin.min(v);
            vmax = vmax.max(v);
        }
    }
    // The grid spacing in equirect pixels bounds what lies between samples.
    let step = {
        let a = p.view_to_equirect(r[0], r[1], fw, fh);
        let b = p.view_to_equirect(r[0] + (r[2] - r[0]) / n as f64, r[1] + (r[3] - r[1]) / n as f64, fw, fh);
        let du = (a[0] - b[0]).abs().min(fw - (a[0] - b[0]).abs());
        du.max((a[1] - b[1]).abs())
    };
    let pad = pad + step * 2.0;
    let inside = |d: [f64; 3]| p.dir_to_view(d).is_some_and(|[x, y]| x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3]);
    let north = inside([0.0, 1.0, 0.0]);
    let south = inside([0.0, -1.0, 0.0]);
    if north {
        vmin = 0.0;
    }
    if south {
        vmax = fh;
    }
    let v0 = ((vmin - pad).floor() as i64).max(0);
    let v1 = ((vmax + pad).ceil() as i64).min(ih as i64);
    if north || south || us.is_empty() {
        return vec![[0, v0, iw as i64, v1]];
    }
    // The shortest arc holding every column: the complement of the largest gap between them.
    us.sort_by(f64::total_cmp);
    let mut gap = (us[0] + fw - us[us.len() - 1], 0usize);
    for k in 1..us.len() {
        let g = us[k] - us[k - 1];
        if g > gap.0 {
            gap = (g, k);
        }
    }
    let start = us[gap.1];
    let len = fw - gap.0;
    if len + 2.0 * pad >= fw {
        return vec![[0, v0, iw as i64, v1]];
    }
    let a = (start - pad).floor() as i64;
    let b = (start + len + pad).ceil() as i64;
    let w = iw as i64;
    let mut out = Vec::new();
    let (a, b) = (a.rem_euclid(w), a.rem_euclid(w) + (b - a));
    if b <= w {
        out.push([a, v0, b, v1]);
    } else {
        out.push([a, v0, w, v1]);
        out.push([0, v0, b - w, v1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, e: f64) -> bool {
        (a - b).abs() <= e
    }

    #[test]
    fn view_centre_looks_along_yaw_and_pitch() {
        let (iw, ih) = (20000.0, 10000.0);
        let p = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 90.0 }, 1000.0, 800.0);
        let [u, v] = p.view_to_equirect(500.0, 400.0, iw, ih);
        assert!(close(u, 10000.0, 1e-6) && close(v, 5000.0, 1e-6), "{u} {v}");
        let p = ViewProjection::new(Camera { yaw: 90.0, pitch: 45.0, fov: 60.0 }, 1000.0, 800.0);
        let [u, v] = p.view_to_equirect(500.0, 400.0, iw, ih);
        assert!(close(u, 15000.0, 1e-6) && close(v, 2500.0, 1e-6), "{u} {v}");
    }

    #[test]
    fn fov_spans_the_view_height() {
        // A 90° vertical FOV: the top edge's centre is 45° up.
        let p = ViewProjection::new(Camera::default(), 1000.0, 800.0);
        let [_, v] = p.view_to_equirect(500.0, 0.0, 3600.0, 1800.0);
        assert!(close(v, 900.0 - 450.0, 1e-6), "{v}");
    }

    #[test]
    fn round_trip_view_equirect_view() {
        let (iw, ih) = (32768.0, 16384.0);
        for cam in [
            Camera { yaw: 0.0, pitch: 0.0, fov: 90.0 },
            Camera { yaw: 179.0, pitch: 10.0, fov: 70.0 },
            Camera { yaw: -135.0, pitch: -80.0, fov: 120.0 },
            Camera { yaw: 33.0, pitch: 89.0, fov: 30.0 },
        ] {
            let p = ViewProjection::new(cam, 1600.0, 900.0);
            for &(x, y) in &[(0.0, 0.0), (1600.0, 900.0), (800.0, 450.0), (123.4, 777.7), (1500.0, 20.0)] {
                let [u, v] = p.view_to_equirect(x, y, iw, ih);
                assert!((0.0..iw).contains(&u) && (0.0..=ih).contains(&v));
                let [bx, by] = p.equirect_to_view(u, v, iw, ih).expect("in front");
                assert!(close(bx, x, 1e-5) && close(by, y, 1e-5), "{cam:?} ({x},{y}) -> ({u},{v}) -> ({bx},{by})");
            }
        }
    }

    #[test]
    fn round_trip_direction_equirect_direction() {
        for &d in &[[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [-0.3, 0.5, -0.8], [0.0, -0.999, 0.01]] {
            let d = normalize(d);
            let [u, v] = dir_to_equirect(d, 4096.0, 2048.0);
            let e = equirect_to_dir(u, v, 4096.0, 2048.0);
            for i in 0..3 {
                assert!(close(d[i], e[i], 1e-9), "{d:?} {e:?}");
            }
        }
    }

    #[test]
    fn seam_wraps_continuously() {
        // Looking at 180°: the view centre is the image's left/right edge, and a pixel either
        // side of it lands at opposite ends of the image.
        let (iw, ih) = (20000.0, 10000.0);
        let p = ViewProjection::new(Camera { yaw: 180.0, pitch: 0.0, fov: 90.0 }, 1000.0, 1000.0);
        let [l, _] = p.view_to_equirect(499.0, 500.0, iw, ih);
        let [r, _] = p.view_to_equirect(501.0, 500.0, iw, ih);
        assert!(l > iw - 50.0 && r < 50.0, "{l} {r}");
        // Equirect u and u + width are the same direction.
        let a = equirect_to_dir(10.0, 3000.0, iw, ih);
        let b = equirect_to_dir(10.0 + iw, 3000.0, iw, ih);
        assert!((0..3).all(|i| close(a[i], b[i], 1e-9)));
        // The dirty rect across the seam splits in two.
        let rects = view_rect_to_equirect(&p, [450.0, 450.0, 550.0, 550.0], 20000, 10000, 1.0);
        assert_eq!(rects.len(), 2, "{rects:?}");
        assert!(rects.iter().any(|r| r[0] == 0) && rects.iter().any(|r| r[2] == 20000), "{rects:?}");
    }

    #[test]
    fn rotation_between_maps_a_to_b_and_is_orthonormal() {
        for (a, b) in [([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]), ([0.3, 0.4, -0.8], [-0.2, 0.9, 0.1]), ([0.0, 1.0, 0.0], [0.0, -1.0, 0.0]), ([1.0, 0.0, 0.0], [1.0, 0.0, 0.0])] {
            let q = rotation_between(a, b);
            let m = rot(&q, normalize(a));
            let b = normalize(b);
            assert!((0..3).all(|i| close(m[i], b[i], 1e-9)), "{a:?}→{b:?}: {m:?}");
            assert!(rot_from_slice(&q.concat()).is_some(), "orthonormal");
        }
    }

    #[test]
    fn wrap_and_clamp() {
        assert_eq!(wrap_deg(190.0), -170.0);
        assert_eq!(wrap_deg(-180.0), 180.0);
        assert_eq!(wrap_deg(540.0), 180.0);
        let c = Camera { yaw: 725.0, pitch: 120.0, fov: 1.0 }.normalized();
        assert!(close(c.yaw, 5.0, 1e-9) && c.pitch == MAX_PITCH && c.fov == MIN_FOV);
    }

    #[test]
    fn pole_in_view_covers_full_width() {
        let p = ViewProjection::new(Camera { yaw: 0.0, pitch: 89.0, fov: 60.0 }, 800.0, 800.0);
        let rects = view_rect_to_equirect(&p, [350.0, 350.0, 450.0, 450.0], 4096, 2048, 1.0);
        assert_eq!(rects.len(), 1);
        assert_eq!((rects[0][0], rects[0][1], rects[0][2]), (0, 0, 4096), "{rects:?}");
    }

    #[test]
    fn dirty_rect_contains_every_pixel_of_the_view_rect() {
        let (iw, ih) = (8000u32, 4000u32);
        let p = ViewProjection::new(Camera { yaw: 40.0, pitch: 30.0, fov: 80.0 }, 1200.0, 800.0);
        let r = [100.0, 200.0, 700.0, 650.0];
        let rects = view_rect_to_equirect(&p, r, iw, ih, 0.0);
        for j in 0..=40 {
            for i in 0..=40 {
                let x = r[0] + (r[2] - r[0]) * i as f64 / 40.0;
                let y = r[1] + (r[3] - r[1]) * j as f64 / 40.0;
                let [u, v] = p.view_to_equirect(x, y, iw as f64, ih as f64);
                assert!(rects.iter().any(|q| u >= q[0] as f64 && u <= q[2] as f64 && v >= q[1] as f64 && v <= q[3] as f64), "({u},{v}) not in {rects:?}");
            }
        }
    }
}

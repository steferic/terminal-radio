// globe.rs — Portable orthographic projection core (no ratatui / IO deps).
//
// This is the terminal twin of the browser's projection.js: the SAME math,
// shareable with an esp-rs firmware crate later. Inputs/outputs are plain
// numbers, working in unit-disc coordinates (x, y in [-1, 1], y pointing up) so
// the caller can map them onto a braille canvas, an e-ink framebuffer, or
// anything else. Keep this file dependency-free.

const DEG: f64 = std::f64::consts::PI / 180.0;

#[derive(Clone, Copy)]
pub struct Center {
    pub lon: f64,
    pub lat: f64,
    pub sin_lat: f64,
    pub cos_lat: f64,
}

impl Center {
    pub fn new(lon_deg: f64, lat_deg: f64) -> Self {
        let lon = lon_deg * DEG;
        let lat = lat_deg * DEG;
        Center { lon, lat, sin_lat: lat.sin(), cos_lat: lat.cos() }
    }
}

#[derive(Clone, Copy)]
pub struct Proj {
    pub x: f64,
    pub y: f64,
    pub front: bool, // true => on the visible near hemisphere
}

// Orthographic azimuthal projection ("globe as seen from space").
pub fn project(lon_deg: f64, lat_deg: f64, c: &Center) -> Proj {
    let lon = lon_deg * DEG;
    let lat = lat_deg * DEG;
    let dlon = lon - c.lon;
    let cos_lat_p = lat.cos();
    let sin_lat_p = lat.sin();
    let cosc = c.sin_lat * sin_lat_p + c.cos_lat * cos_lat_p * dlon.cos();
    let x = cos_lat_p * dlon.sin();
    let y = c.cos_lat * sin_lat_p - c.sin_lat * cos_lat_p * dlon.cos();
    Proj { x, y, front: cosc >= 0.0 }
}

// Shortest signed delta between two longitudes (handles the ±180 seam).
pub fn shortest_lon_delta(from: f64, to: f64) -> f64 {
    let mut d = (to - from) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

// Built-in graticule (meridians + parallels), generated not stored.
pub fn graticule(step: i32) -> Vec<Vec<(f64, f64)>> {
    let mut out = Vec::new();
    let mut lon = -180;
    while lon < 180 {
        let mut line = Vec::new();
        let mut lat = -80;
        while lat <= 80 {
            line.push((lon as f64, lat as f64));
            lat += 4;
        }
        out.push(line);
        lon += step;
    }
    let mut lat = -60;
    while lat <= 60 {
        let mut line = Vec::new();
        let mut lon = -180;
        while lon <= 180 {
            line.push((lon as f64, lat as f64));
            lon += 4;
        }
        out.push(line);
        lat += step;
    }
    out
}

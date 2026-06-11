use bevy::prelude::*;

/// Height of one terrain level in world units.
pub const LEVEL: f32 = 0.5;
/// Tiles within this distance of surface water count as irrigated.
pub const IRRIGATION_RANGE: u32 = 5;

/// The tile grid: terrain heights, water depths and per-tile occupancy.
/// This is the single source of truth for the world; rendering and UI react to it.
#[derive(Resource)]
pub struct Map {
    pub width: u32,
    pub height: u32,
    /// Terrain height in levels (0 = river bed).
    pub ground: Vec<i32>,
    /// Water depth in levels (f32, can be fractional).
    pub water: Vec<f32>,
    /// Water source tiles (river inlet).
    pub source: Vec<bool>,
    /// Map-edge tiles where water drains away (river outlet).
    pub drain: Vec<bool>,
    /// Dams raise the effective wall height so water piles up behind them.
    pub dam: Vec<bool>,
    /// Tile is within irrigation range of surface water.
    pub irrigated: Vec<bool>,
    /// Building occupying this tile, if any.
    pub building: Vec<Option<Entity>>,
    /// Tree on this tile, if any.
    pub tree: Vec<Option<Entity>>,
}

impl Map {
    pub fn generate(width: u32, height: u32) -> Self {
        Self::generate_seeded(width, height, 0.0)
    }

    pub fn generate_seeded(width: u32, height: u32, seed: f32) -> Self {
        let n = (width * height) as usize;
        let mut map = Self {
            width,
            height,
            ground: vec![1; n],
            water: vec![0.0; n],
            source: vec![false; n],
            drain: vec![false; n],
            dam: vec![false; n],
            irrigated: vec![false; n],
            building: vec![None; n],
            tree: vec![None; n],
        };

        // Gentle hills from cheap value noise.
        for y in 0..height {
            for x in 0..width {
                let h = 1.0
                    + 1.6 * noise(x as f32 * 0.09 + seed * 3.7, y as f32 * 0.09 + seed * 1.3)
                    + 0.8 * noise(x as f32 * 0.23 + 31.0 + seed, y as f32 * 0.23 + 17.0);
                let i = map.idx(x, y);
                map.ground[i] = h.round().clamp(1.0, 4.0) as i32;
            }
        }

        // Carve a meandering river (west to east), 2 tiles wide, at level 0.
        for x in 0..width {
            let center = (height as f32 / 2.0
                + (x as f32 * 0.18 + seed * 0.61).sin() * (height as f32 * 0.14))
                as i32;
            for dy in -1..=1 {
                let y = (center + dy).clamp(0, height as i32 - 1) as u32;
                let i = map.idx(x, y);
                map.ground[i] = 0;
                if dy != 1 {
                    // 2-wide water channel; the third carved row is the bank.
                    if x == 0 {
                        map.source[i] = true;
                    }
                    if x == width - 1 {
                        map.drain[i] = true;
                    }
                    map.water[i] = 1.0;
                }
            }
        }
        map
    }

    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        (y * self.width + x) as usize
    }

    #[inline]
    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    /// World-space center of a tile's ground surface.
    pub fn tile_center(&self, x: u32, y: u32) -> Vec3 {
        let g = self.ground[self.idx(x, y)] as f32 * LEVEL;
        Vec3::new(
            x as f32 - self.width as f32 / 2.0 + 0.5,
            g,
            y as f32 - self.height as f32 / 2.0 + 0.5,
        )
    }

    /// Tile coordinates for a world position, if inside the map.
    pub fn tile_at(&self, pos: Vec3) -> Option<UVec2> {
        let x = (pos.x + self.width as f32 / 2.0).floor() as i32;
        let y = (pos.z + self.height as f32 / 2.0).floor() as i32;
        self.in_bounds(x, y).then(|| UVec2::new(x as u32, y as u32))
    }

    pub fn has_water(&self, x: u32, y: u32) -> bool {
        self.water[self.idx(x, y)] > 0.05
    }

    pub fn is_river_bed(&self, x: u32, y: u32) -> bool {
        self.ground[self.idx(x, y)] == 0
    }

    /// A land tile that is free of water, buildings and trees.
    pub fn is_free_land(&self, x: u32, y: u32) -> bool {
        let i = self.idx(x, y);
        self.ground[i] >= 1
            && self.water[i] <= 0.05
            && self.building[i].is_none()
            && self.tree[i].is_none()
    }

    pub fn neighbors4(&self, x: u32, y: u32) -> impl Iterator<Item = UVec2> + '_ {
        [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .into_iter()
            .filter_map(move |(dx, dy)| {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                self.in_bounds(nx, ny)
                    .then(|| UVec2::new(nx as u32, ny as u32))
            })
    }

    pub fn adjacent_to_water(&self, x: u32, y: u32) -> bool {
        self.neighbors4(x, y).any(|n| self.has_water(n.x, n.y))
    }
}

/// Cheap deterministic 2D value noise in [-1, 1].
fn noise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (xf, yf) = (x - xi, y - yi);
    let s = |a: f32, b: f32| {
        let h = (a * 127.1 + b * 311.7).sin() * 43758.547;
        h - h.floor()
    };
    let (a, b, c, d) = (
        s(xi, yi),
        s(xi + 1.0, yi),
        s(xi, yi + 1.0),
        s(xi + 1.0, yi + 1.0),
    );
    let (u, v) = (xf * xf * (3.0 - 2.0 * xf), yf * yf * (3.0 - 2.0 * yf));
    (a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v) * 2.0 - 1.0
}

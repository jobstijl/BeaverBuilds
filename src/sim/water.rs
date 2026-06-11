use bevy::prelude::*;

use super::Season;
use super::map::{IRRIGATION_RANGE, Map};

/// Simplified Timberborn-style water: a cellular automaton over the tile grid.
/// Each fixed tick, water equalizes towards neighboring tiles with a lower
/// water *surface* (ground + depth). Dams raise the spill height of a tile so
/// water piles up behind them. During a drought the source stops and
/// evaporation ramps up.
pub struct WaterPlugin;

/// Label for the water simulation; the render mirror runs after this.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WaterSet;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            FixedUpdate,
            (flow_water, update_irrigation).chain().in_set(WaterSet),
        );
    }
}

const SOURCE_RATE: f32 = 3.0;
const MAX_SOURCE_DEPTH: f32 = 1.6;
const FLOW_RATE: f32 = 4.0;
const EVAPORATION_WET: f32 = 0.002;
// Tuned so a dammed pool outlasts a full drought: total drought
// evaporation (rate x DROUGHT_LENGTH = 0.6 depth) must stay well below the
// pool depth a dam can hold. The dams_improve_drought_retention test pins
// this relationship.
const EVAPORATION_DROUGHT: f32 = 0.02;
const DAM_HEIGHT: f32 = 1.2;

/// One cellular-automaton step over a water grid. Pure — also runs on the
/// async task pool for the drought forecast.
#[allow(clippy::too_many_arguments)]
pub fn step_water(
    ground: &[i32],
    dam: &[bool],
    drain: &[bool],
    source: Option<&[bool]>,
    water: &mut [f32],
    width: u32,
    height: u32,
    dt: f32,
    evaporation: f32,
) {
    let idx = |x: u32, y: u32| (y * width + x) as usize;

    // Inflow at sources (wet season only).
    if let Some(source) = source {
        for i in 0..water.len() {
            if source[i] {
                water[i] = (water[i] + SOURCE_RATE * dt).min(MAX_SOURCE_DEPTH);
            }
        }
    }

    // Equalize each tile with its east and south neighbor (covers every pair once).
    let mut delta = vec![0.0f32; water.len()];
    for y in 0..height {
        for x in 0..width {
            let i = idx(x, y);
            for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                if nx >= width || ny >= height {
                    continue;
                }
                let j = idx(nx, ny);
                let si = ground[i] as f32 + water[i];
                let sj = ground[j] as f32 + water[j];
                let (from, to, sf, st) = if si >= sj {
                    (i, j, si, sj)
                } else {
                    (j, i, sj, si)
                };
                // A dam on either side raises the spill height.
                let wall = ground[from].max(ground[to]) as f32
                    + if dam[to] || dam[from] {
                        DAM_HEIGHT
                    } else {
                        0.0
                    };
                if sf <= wall {
                    continue;
                }
                let head = (sf - st.max(wall)).max(0.0);
                let amount = (head * 0.5 * FLOW_RATE * dt)
                    .min(water[from] * 0.25)
                    .max(0.0);
                delta[from] -= amount;
                delta[to] += amount;
            }
        }
    }

    for i in 0..water.len() {
        water[i] = (water[i] + delta[i] - evaporation * dt).max(0.0);
        if drain[i] {
            // The river flows off the map here.
            water[i] = (water[i] - FLOW_RATE * 0.4 * dt).max(0.0);
        }
    }
}

/// Fraction of the current surface water that would survive a full drought,
/// found by running the cellular automaton forward without inflow. Pure and
/// owning — intended to run on the [`AsyncComputeTaskPool`] for the
/// reactive drought-forecast readout.
///
/// [`AsyncComputeTaskPool`]: bevy::tasks::AsyncComputeTaskPool
#[allow(clippy::too_many_arguments)]
pub fn forecast_drought_retention(
    ground: Vec<i32>,
    dam: Vec<bool>,
    drain: Vec<bool>,
    mut water: Vec<f32>,
    width: u32,
    height: u32,
    drought_secs: f32,
) -> f32 {
    let before: f32 = water.iter().sum();
    if before < 1.0 {
        return 0.0;
    }
    let dt = 1.0 / 16.0;
    let steps = (drought_secs / dt) as usize;
    for _ in 0..steps {
        step_water(
            &ground,
            &dam,
            &drain,
            None,
            &mut water,
            width,
            height,
            dt,
            EVAPORATION_DROUGHT,
        );
    }
    water.iter().sum::<f32>() / before
}

fn flow_water(time: Res<Time>, mut map: ResMut<Map>, season: Res<Season>) {
    let dt = time.delta_secs();
    let map = &mut *map;
    let evaporation = if season.drought {
        EVAPORATION_DROUGHT
    } else {
        EVAPORATION_WET
    };
    let source = (!season.drought).then_some(map.source.as_slice());
    step_water(
        &map.ground,
        &map.dam,
        &map.drain,
        source,
        &mut map.water,
        map.width,
        map.height,
        dt,
        evaporation,
    );
}

/// Flood-fill irrigation outwards from wet tiles. Cheap enough to run every
/// fixed tick at this map size, and keeping it in lockstep with the water sim
/// avoids one-frame-stale farmland.
fn update_irrigation(mut map: ResMut<Map>) {
    let map = &mut *map;
    let mut dist = vec![u32::MAX; map.water.len()];
    let mut queue = std::collections::VecDeque::new();
    for y in 0..map.height {
        for x in 0..map.width {
            let i = map.idx(x, y);
            if map.water[i] > 0.05 {
                dist[i] = 0;
                queue.push_back(UVec2::new(x, y));
            }
        }
    }
    while let Some(p) = queue.pop_front() {
        let d = dist[map.idx(p.x, p.y)];
        if d >= IRRIGATION_RANGE {
            continue;
        }
        for n in map.neighbors4(p.x, p.y).collect::<Vec<_>>() {
            let ni = map.idx(n.x, n.y);
            if dist[ni] > d + 1 {
                dist[ni] = d + 1;
                queue.push_back(n);
            }
        }
    }
    for (i, d) in dist.iter().enumerate() {
        map.irrigated[i] = *d <= IRRIGATION_RANGE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_conserves_water_without_sinks() {
        let (w, h) = (6u32, 4u32);
        let n = (w * h) as usize;
        let ground: Vec<i32> = (0..n).map(|i| ((i * 7) % 3) as i32).collect();
        let mut water: Vec<f32> = (0..n).map(|i| ((i * 13) % 5) as f32 * 0.3).collect();
        let before: f32 = water.iter().sum();
        let no = vec![false; n];
        for _ in 0..50 {
            step_water(&ground, &no, &no, None, &mut water, w, h, 1.0 / 16.0, 0.0);
        }
        let after: f32 = water.iter().sum();
        assert!(
            (before - after).abs() < 1e-3,
            "no sources/drains/evaporation: {before} -> {after}"
        );
        assert!(water.iter().all(|d| *d >= 0.0));
    }

    /// The gameplay invariant behind the dam: building one in the channel
    /// must improve the drought forecast.
    #[test]
    fn dams_improve_drought_retention() {
        let (w, h) = (10u32, 3u32);
        let n = (w * h) as usize;
        // A channel at y=1 (ground 0) draining off the right edge; banks at 1.
        let mut ground = vec![1i32; n];
        let mut water = vec![0.0f32; n];
        let mut drain = vec![false; n];
        for x in 0..w {
            let i = (w + x) as usize; // y = 1
            ground[i] = 0;
            water[i] = 1.0;
        }
        drain[(w + w - 1) as usize] = true;

        let no_dam = vec![false; n];
        let without = forecast_drought_retention(
            ground.clone(),
            no_dam,
            drain.clone(),
            water.clone(),
            w,
            h,
            30.0,
        );
        let mut dam = vec![false; n];
        dam[(w + w - 3) as usize] = true; // dam in the channel, before the drain
        let with = forecast_drought_retention(ground, dam, drain, water, w, h, 30.0);
        assert!(
            with > without + 0.05,
            "dam must retain noticeably more water: {without:.3} -> {with:.3}"
        );
        assert!((0.0..=1.0).contains(&with) && (0.0..=1.0).contains(&without));
    }
}

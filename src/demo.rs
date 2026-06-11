//! Attract mode (`BB_DEMO=1`): a rule-based governor plays the colony so
//! you can watch scenarios unfold hands-off. Every few seconds it takes one
//! deliberate action — bootstrapping the economy, housing the population,
//! damming the river when the (async) drought forecast looks grim, and
//! paving roads tile-by-tile toward its newest buildings. The camera orbits
//! slowly.

use bevy::prelude::*;
use bevy_reactive_bsn::AsyncValue;

use crate::render::camera::CameraRig;
use crate::sim::buildings::{
    Building, BuildingKind, UnderConstruction, def, place_building, placement_error,
};
use crate::sim::map::Map;
use crate::sim::trees::Tree;
use crate::sim::{Population, Stockpile};

pub struct DemoPlugin;

impl Plugin for DemoPlugin {
    fn build(&self, app: &mut App) {
        if std::env::var("BB_DEMO").is_err() {
            return;
        }
        app.init_resource::<Governor>()
            .add_systems(Update, (govern, orbit_camera));
    }
}

#[derive(Resource, Default)]
struct Governor {
    cooldown: f32,
    /// Pending road: tiles still to pave, one per action.
    road: Vec<UVec2>,
}

fn orbit_camera(time: Res<Time>, mut rig: ResMut<CameraRig>) {
    rig.yaw += time.delta_secs() * 0.05;
}

/// One action every few seconds, by priority. Skips a beat when it can't
/// afford its choice — logs are the pacing resource, just like for a player.
#[allow(clippy::too_many_arguments)]
fn govern(
    mut commands: Commands,
    time: Res<Time>,
    mut governor: ResMut<Governor>,
    mut map: ResMut<Map>,
    mut stockpile: ResMut<Stockpile>,
    population: Res<Population>,
    buildings: Query<&Building>,
    constructing: Query<(), With<UnderConstruction>>,
    trees: Query<&Tree>,
    forecast: Query<&AsyncValue<f32>>,
) {
    governor.cooldown -= time.delta_secs();
    if governor.cooldown > 0.0 {
        return;
    }
    governor.cooldown = 3.5;
    // Let construction crews catch up before queuing more.
    if constructing.iter().count() >= 2 {
        return;
    }

    let count = |kind: BuildingKind| buildings.iter().filter(|b| b.kind == kind).count();
    let anchor = colony_anchor(&map, &buildings);
    let retention = forecast.iter().find_map(|v| v.ready().copied());

    // Survival first (water, food, the economy that pays for both), then
    // growth, then drought-proofing.
    let choice = if count(BuildingKind::Lumberjack) == 0 {
        Some(BuildingKind::Lumberjack)
    } else if count(BuildingKind::WaterPump) == 0 {
        Some(BuildingKind::WaterPump)
    } else if count(BuildingKind::CarrotFarm) == 0 {
        Some(BuildingKind::CarrotFarm)
    } else if stockpile.water < 10.0 && count(BuildingKind::WaterPump) < 4 {
        Some(BuildingKind::WaterPump)
    } else if stockpile.food < 10.0 && count(BuildingKind::CarrotFarm) < 4 {
        Some(BuildingKind::CarrotFarm)
    } else if count(BuildingKind::Lumberjack) < 2 && stockpile.logs < 25.0 {
        Some(BuildingKind::Lumberjack)
    } else if population.count >= population.cap {
        Some(BuildingKind::Lodge)
    } else if retention.is_some_and(|r| r < 0.4) && count(BuildingKind::Dam) < 2 {
        // At most two dams: enough to hold a reserve without strangling the
        // downstream flow the pumps drink from (a lesson the governor
        // learned the hard way).
        Some(BuildingKind::Dam)
    } else if count(BuildingKind::Forester) == 0 && trees.iter().count() < 200 {
        Some(BuildingKind::Forester)
    } else {
        None
    };

    if let Some(mut kind) = choice {
        if stockpile.logs < def(kind).cost_logs {
            // Can't afford the priority — restart the economy with a cheap
            // lumberjack instead of idling into a death spiral.
            if count(BuildingKind::Lumberjack) < 3
                && stockpile.logs >= def(BuildingKind::Lumberjack).cost_logs
            {
                kind = BuildingKind::Lumberjack;
            } else {
                return; // save up; try again next beat
            }
        }
        if let Some(tile) = find_spot(&map, kind, anchor, &trees) {
            let entity = place_building(&mut commands, &mut map, &mut stockpile, kind, tile);
            let _ = entity;
            info!("demo governor: building {} at {tile}", def(kind).name);
            // Queue a road from the colony toward production buildings.
            if matches!(
                kind,
                BuildingKind::Lumberjack | BuildingKind::WaterPump | BuildingKind::CarrotFarm
            ) {
                governor.road = line_between(anchor, tile);
            }
        }
        return;
    }

    // Idle beat: pave the next road tile, if any and affordable.
    while let Some(tile) = governor.road.pop() {
        if stockpile.logs < def(BuildingKind::Path).cost_logs {
            governor.road.push(tile);
            return;
        }
        if placement_error(&map, BuildingKind::Path, tile).is_none() {
            place_building(
                &mut commands,
                &mut map,
                &mut stockpile,
                BuildingKind::Path,
                tile,
            );
            return; // one tile per beat: roads grow visibly
        }
    }
}

/// Where the colony "is": the first lodge, else the map center.
fn colony_anchor(map: &Map, buildings: &Query<&Building>) -> UVec2 {
    buildings
        .iter()
        .find(|b| b.kind == BuildingKind::Lodge)
        .map(|b| b.tile)
        .unwrap_or(UVec2::new(map.width / 2, map.height / 2))
}

/// Nearest tile to `anchor` where `kind` may be placed, with kind-specific
/// preferences (lumberjacks want mature trees in range, farms want
/// irrigation — already enforced by `placement_error` where applicable).
fn find_spot(map: &Map, kind: BuildingKind, anchor: UVec2, trees: &Query<&Tree>) -> Option<UVec2> {
    let mut candidates: Vec<UVec2> = (0..map.height)
        .flat_map(|y| (0..map.width).map(move |x| UVec2::new(x, y)))
        .filter(|t| placement_error(map, kind, *t).is_none())
        .collect();
    if kind == BuildingKind::CarrotFarm {
        candidates.retain(|t| map.irrigated[map.idx(t.x, t.y)]);
    }
    if kind == BuildingKind::Lumberjack {
        let mature: Vec<UVec2> = trees
            .iter()
            .filter(|t| t.is_mature())
            .map(|t| t.tile)
            .collect();
        candidates.retain(|t| {
            mature
                .iter()
                .any(|m| m.as_ivec2().distance_squared(t.as_ivec2()) <= 25)
        });
    }
    candidates
        .into_iter()
        .min_by_key(|t| t.as_ivec2().distance_squared(anchor.as_ivec2()))
}

/// Integer line between two tiles (excluding endpoints), reversed so
/// `pop()` paves outward from the colony.
fn line_between(from: UVec2, to: UVec2) -> Vec<UVec2> {
    let (mut x, mut y) = (from.x as i32, from.y as i32);
    let (tx, ty) = (to.x as i32, to.y as i32);
    let mut tiles = Vec::new();
    while (x, y) != (tx, ty) {
        let (dx, dy) = (tx - x, ty - y);
        if dx.abs() >= dy.abs() {
            x += dx.signum();
        } else {
            y += dy.signum();
        }
        if (x, y) != (tx, ty) {
            tiles.push(UVec2::new(x as u32, y as u32));
        }
    }
    tiles.reverse();
    tiles
}

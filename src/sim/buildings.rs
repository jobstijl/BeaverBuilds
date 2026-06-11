use bevy::prelude::*;

use super::beavers::{Job, JobKind};
use super::map::Map;
use super::trees::Tree;
use super::{Population, Stockpile};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum BuildingKind {
    Lodge,
    WaterPump,
    CarrotFarm,
    Lumberjack,
    Forester,
    Dam,
    Path,
}

pub struct BuildingDef {
    pub kind: BuildingKind,
    pub name: &'static str,
    pub description: &'static str,
    pub cost_logs: f32,
    pub build_work: f32,
    /// Work radius in tiles, for buildings that operate on the surroundings.
    pub radius: u32,
}

pub const BUILDING_DEFS: &[BuildingDef] = &[
    BuildingDef {
        kind: BuildingKind::Lodge,
        name: "Lodge",
        description: "Houses 5 beavers. New beavers arrive while food and water last.",
        cost_logs: 20.0,
        build_work: 10.0,
        radius: 0,
    },
    BuildingDef {
        kind: BuildingKind::WaterPump,
        name: "Water pump",
        description: "Pumps drinking water. Must stand next to water.",
        cost_logs: 12.0,
        build_work: 6.0,
        radius: 0,
    },
    BuildingDef {
        kind: BuildingKind::CarrotFarm,
        name: "Carrot farm",
        description: "Grows food. The tile must be irrigated (green).",
        cost_logs: 10.0,
        build_work: 6.0,
        radius: 0,
    },
    BuildingDef {
        kind: BuildingKind::Lumberjack,
        name: "Lumberjack flag",
        description: "Beavers chop mature trees nearby.",
        cost_logs: 5.0,
        build_work: 3.0,
        radius: 6,
    },
    BuildingDef {
        kind: BuildingKind::Forester,
        name: "Forester",
        description: "Plants new trees on free land nearby.",
        cost_logs: 8.0,
        build_work: 5.0,
        radius: 5,
    },
    BuildingDef {
        kind: BuildingKind::Dam,
        name: "Dam",
        description: "Holds water back. Build in the river to store water for droughts.",
        cost_logs: 6.0,
        build_work: 5.0,
        radius: 0,
    },
    BuildingDef {
        kind: BuildingKind::Path,
        name: "Path",
        description: "A stone path. Beavers walk much faster on it.",
        cost_logs: 1.0,
        build_work: 0.8,
        radius: 0,
    },
];

pub fn def(kind: BuildingKind) -> &'static BuildingDef {
    BUILDING_DEFS.iter().find(|d| d.kind == kind).unwrap()
}

#[derive(Component)]
pub struct Building {
    pub kind: BuildingKind,
    pub tile: UVec2,
}

/// Present while a building still needs builder work.
#[derive(Component)]
pub struct UnderConstruction {
    pub done: f32,
    pub required: f32,
}

/// Periodic production state for active buildings.
#[derive(Component, Default)]
pub struct WorkCycle {
    pub cooldown: f32,
    /// A pending job entity, if one is currently posted.
    pub job: Option<Entity>,
}

pub struct BuildingsPlugin;

impl Plugin for BuildingsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            FixedUpdate,
            (post_production_jobs, lodge_spawning, update_population),
        );
    }
}

/// Validity of placing `kind` on the given tile. Returns an error string for the UI.
pub fn placement_error(map: &Map, kind: BuildingKind, tile: UVec2) -> Option<&'static str> {
    let (x, y) = (tile.x, tile.y);
    let i = map.idx(x, y);
    if map.building[i].is_some() {
        return Some("occupied");
    }
    match kind {
        BuildingKind::Dam => {
            if !map.is_river_bed(x, y) {
                return Some("must be built in the river bed");
            }
        }
        BuildingKind::WaterPump => {
            if !map.is_free_land(x, y) {
                return Some("needs free land");
            }
            if !map.adjacent_to_water(x, y) {
                return Some("must stand next to water");
            }
        }
        _ => {
            if !map.is_free_land(x, y) {
                return Some("needs free land");
            }
        }
    }
    None
}

/// Place a building (assumes validity and affordability were checked).
pub fn place_building(
    commands: &mut Commands,
    map: &mut Map,
    stockpile: &mut Stockpile,
    kind: BuildingKind,
    tile: UVec2,
) -> Entity {
    let d = def(kind);
    stockpile.logs -= d.cost_logs;
    let entity = commands
        .spawn((
            Building { kind, tile },
            UnderConstruction {
                done: 0.0,
                required: d.build_work,
            },
            WorkCycle::default(),
        ))
        .id();
    let i = map.idx(tile.x, tile.y);
    map.building[i] = Some(entity);
    if kind == BuildingKind::Dam {
        map.dam[i] = true;
    }
    // Builders need to come and construct it.
    commands.spawn(Job {
        kind: JobKind::Build(entity),
        tile,
        work: d.build_work,
        claimed_by: None,
    });
    entity
}

pub fn demolish(
    commands: &mut Commands,
    map: &mut Map,
    stockpile: &mut Stockpile,
    entity: Entity,
    building: &Building,
) {
    let i = map.idx(building.tile.x, building.tile.y);
    map.building[i] = None;
    if building.kind == BuildingKind::Dam {
        map.dam[i] = false;
    }
    stockpile.logs += def(building.kind).cost_logs * 0.5;
    commands.entity(entity).despawn();
}

/// Active production buildings periodically post jobs for beavers.
fn post_production_jobs(
    mut commands: Commands,
    time: Res<Time>,
    map: Res<Map>,
    jobs: Query<(), With<Job>>,
    mut buildings: Query<(Entity, &Building, &mut WorkCycle), Without<UnderConstruction>>,
    trees: Query<&Tree>,
) {
    let dt = time.delta_secs();
    for (entity, building, mut cycle) in &mut buildings {
        // Still waiting on a previously posted job?
        if let Some(job) = cycle.job {
            if jobs.contains(job) {
                continue;
            }
            cycle.job = None;
        }
        cycle.cooldown -= dt;
        if cycle.cooldown > 0.0 {
            continue;
        }
        let d = def(building.kind);
        let (x, y) = (building.tile.x, building.tile.y);
        let job = match building.kind {
            BuildingKind::WaterPump if map.adjacent_to_water(x, y) => {
                Some((JobKind::Pump(entity), building.tile, 3.0))
            }
            BuildingKind::CarrotFarm if map.irrigated[map.idx(x, y)] => {
                Some((JobKind::Farm(entity), building.tile, 5.0))
            }
            BuildingKind::Lumberjack => find_mature_tree(&map, building.tile, d.radius, &trees)
                .map(|(tree, tile)| (JobKind::Chop(tree), tile, 4.0)),
            BuildingKind::Forester => find_plant_spot(&map, building.tile, d.radius)
                .map(|tile| (JobKind::Plant(tile), tile, 3.0)),
            _ => None,
        };
        if let Some((kind, tile, work)) = job {
            let job = commands
                .spawn(Job {
                    kind,
                    tile,
                    work,
                    claimed_by: None,
                })
                .id();
            cycle.job = Some(job);
            cycle.cooldown = 2.0;
        } else {
            // Nothing to do right now; check again in a moment.
            cycle.cooldown = 1.5;
        }
    }
}

fn tiles_in_radius(map: &Map, center: UVec2, radius: u32) -> impl Iterator<Item = UVec2> + '_ {
    let r = radius as i32;
    (-r..=r).flat_map(move |dy| {
        (-r..=r).filter_map(move |dx| {
            let (x, y) = (center.x as i32 + dx, center.y as i32 + dy);
            (dx * dx + dy * dy <= r * r && map.in_bounds(x, y))
                .then(|| UVec2::new(x as u32, y as u32))
        })
    })
}

fn find_mature_tree(
    map: &Map,
    center: UVec2,
    radius: u32,
    trees: &Query<&Tree>,
) -> Option<(Entity, UVec2)> {
    tiles_in_radius(map, center, radius).find_map(|t| {
        let entity = map.tree[map.idx(t.x, t.y)]?;
        let tree = trees.get(entity).ok()?;
        tree.is_mature().then_some((entity, t))
    })
}

fn find_plant_spot(map: &Map, center: UVec2, radius: u32) -> Option<UVec2> {
    tiles_in_radius(map, center, radius).find(|t| map.is_free_land(t.x, t.y))
}

/// Lodges attract new beavers while there is housing, food and water.
fn lodge_spawning(
    mut commands: Commands,
    time: Res<Time>,
    map: Res<Map>,
    population: Res<Population>,
    stockpile: Res<Stockpile>,
    mut lodges: Query<(&Building, &mut WorkCycle), Without<UnderConstruction>>,
) {
    let dt = time.delta_secs();
    for (building, mut cycle) in &mut lodges {
        if building.kind != BuildingKind::Lodge {
            continue;
        }
        cycle.cooldown -= dt;
        if cycle.cooldown > 0.0 {
            continue;
        }
        cycle.cooldown = 25.0;
        if population.count < population.cap && stockpile.food >= 3.0 && stockpile.water >= 3.0 {
            let pos = map.tile_center(building.tile.x, building.tile.y);
            super::beavers::spawn_beaver(&mut commands, pos);
        }
    }
}

fn update_population(
    mut population: ResMut<Population>,
    beavers: Query<(), With<super::beavers::Beaver>>,
    lodges: Query<&Building, Without<UnderConstruction>>,
) {
    let count = beavers.iter().count() as u32;
    let cap = 3 + lodges
        .iter()
        .filter(|b| b.kind == BuildingKind::Lodge)
        .count() as u32
        * 5;
    // Only write (and trip change detection) when something actually changed.
    if population.count != count || population.cap != cap {
        population.count = count;
        population.cap = cap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_rules() {
        let map = Map::generate(24, 24);
        let riverbed = (0..24u32)
            .flat_map(|y| (0..24u32).map(move |x| UVec2::new(x, y)))
            .find(|t| map.is_river_bed(t.x, t.y))
            .expect("generated map has a river");
        let dry_far = (0..24u32)
            .flat_map(|y| (0..24u32).map(move |x| UVec2::new(x, y)))
            .find(|t| {
                map.is_free_land(t.x, t.y)
                    && !map.adjacent_to_water(t.x, t.y)
                    && !map.irrigated[map.idx(t.x, t.y)]
            })
            .expect("some dry land exists");
        let shore = (0..24u32)
            .flat_map(|y| (0..24u32).map(move |x| UVec2::new(x, y)))
            .find(|t| map.is_free_land(t.x, t.y) && map.adjacent_to_water(t.x, t.y))
            .expect("some shore exists");

        assert!(placement_error(&map, BuildingKind::Dam, riverbed).is_none());
        assert!(placement_error(&map, BuildingKind::Dam, dry_far).is_some());
        assert!(placement_error(&map, BuildingKind::Lodge, riverbed).is_some());
        assert!(placement_error(&map, BuildingKind::Lodge, dry_far).is_none());
        assert!(placement_error(&map, BuildingKind::WaterPump, dry_far).is_some());
        assert!(placement_error(&map, BuildingKind::WaterPump, shore).is_none());
    }
}

use bevy::prelude::*;
use bevy_reactive_bsn::{AsyncSlot, AsyncValue};

use super::Stockpile;
use super::buildings::{Building, BuildingKind, UnderConstruction};
use super::map::Map;
use super::pathfind::{self, GRASS_COST, PATH_COST, WalkGrid};
use super::trees::{Tree, spawn_tree};

/// An async pathfinding result, tagged with the job it was computed for so
/// stale routes are ignored. Lands on the beaver as `AsyncValue<ComputedPath>`
/// (the layer's async machinery, consumed here by a plain system).
pub struct ComputedPath {
    pub job: Entity,
    pub tiles: Option<Vec<UVec2>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BeaverState {
    Idle,
    Goto(Entity),
    Work(Entity),
}

#[derive(Component)]
pub struct Beaver {
    pub state: BeaverState,
    pub hunger: f32,
    pub thirst: f32,
    pub starving: f32,
}

impl Default for Beaver {
    fn default() -> Self {
        Self {
            state: BeaverState::Idle,
            hunger: 0.0,
            thirst: 0.0,
            starving: 0.0,
        }
    }
}

/// Marker inserted while a beaver lacks food or water. Kept as a separate
/// component (rather than a field) so presence/absence is the change signal:
/// visual reactors wake on the transition, not on every hunger tick.
#[derive(Component)]
pub struct Starving;

/// Derived state: how many beavers are currently starving. Maintained at the
/// `Starving` transitions so UI reactors can depend on one resource instead
/// of scanning beavers.
#[derive(Resource, Default)]
pub struct StarvingCount(pub u32);

/// A task posted on the colony job board, physically located at `tile`.
#[derive(Component)]
pub struct Job {
    pub kind: JobKind,
    pub tile: UVec2,
    pub work: f32,
    pub claimed_by: Option<Entity>,
}

#[derive(Clone, Copy)]
pub enum JobKind {
    /// Construct the building entity.
    Build(Entity),
    /// Chop the tree entity.
    Chop(Entity),
    /// One pumping cycle at the water pump.
    Pump(Entity),
    /// One harvest cycle at the farm.
    Farm(Entity),
    /// Plant a sapling on the tile.
    Plant(UVec2),
}

const WALK_SPEED: f32 = 2.6;
const HUNGER_RATE: f32 = 1.0 / 35.0;
const THIRST_RATE: f32 = 1.0 / 30.0;
const STARVE_LIMIT: f32 = 45.0;

pub struct BeaversPlugin;

impl Plugin for BeaversPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StarvingCount>()
            .add_systems(Startup, initial_colony)
            .add_systems(
                FixedUpdate,
                (validate_jobs, claim_jobs, move_beavers, work_jobs, needs).chain(),
            );
    }
}

pub fn spawn_beaver(commands: &mut Commands, pos: Vec3) {
    commands.spawn((Beaver::default(), Transform::from_translation(pos)));
}

pub fn initial_colony(mut commands: Commands, map: Res<Map>) {
    // Start with three beavers on free land closest to the map center.
    let center = UVec2::new(map.width / 2, map.height / 2);
    let mut spawned = 0;
    'search: for r in 0..map.width.min(map.height) / 2 {
        for y in center.y.saturating_sub(r)..=(center.y + r).min(map.height - 1) {
            for x in center.x.saturating_sub(r)..=(center.x + r).min(map.width - 1) {
                if map.is_free_land(x, y) {
                    let mut pos = map.tile_center(x, y);
                    pos.x += (spawned as f32 - 1.0) * 0.3;
                    spawn_beaver(&mut commands, pos);
                    spawned += 1;
                    if spawned == 3 {
                        break 'search;
                    }
                }
            }
        }
    }
}

/// Drop jobs whose target no longer exists or whose precondition vanished.
type JobTargets = Or<(With<Building>, With<Tree>)>;

fn validate_jobs(
    mut commands: Commands,
    map: Res<Map>,
    jobs: Query<(Entity, &Job)>,
    targets: Query<(), JobTargets>,
) {
    for (entity, job) in &jobs {
        let valid = match job.kind {
            JobKind::Build(t) | JobKind::Chop(t) | JobKind::Pump(t) | JobKind::Farm(t) => {
                targets.contains(t)
            }
            JobKind::Plant(tile) => map.is_free_land(tile.x, tile.y),
        };
        if !valid {
            commands.entity(entity).despawn();
        }
    }
}

/// Traversal-cost snapshot for the async pathfinder: deep water, trees and
/// buildings block; finished stone paths are markedly cheaper than grass,
/// so computed routes bend along the road network.
pub(crate) fn walk_grid(
    map: &Map,
    buildings: &Query<(&Building, Has<UnderConstruction>)>,
) -> WalkGrid {
    let n = (map.width * map.height) as usize;
    let mut cost = vec![GRASS_COST; n];
    for (i, cost) in cost.iter_mut().enumerate() {
        if map.water[i] > 0.35 || map.tree[i].is_some() {
            *cost = f32::INFINITY;
        }
    }
    for (building, under_construction) in buildings {
        let i = map.idx(building.tile.x, building.tile.y);
        cost[i] = match building.kind {
            BuildingKind::Path if !under_construction => PATH_COST,
            BuildingKind::Path => GRASS_COST,
            _ => f32::INFINITY,
        };
    }
    WalkGrid {
        width: map.width,
        height: map.height,
        cost,
    }
}

fn claim_jobs(
    mut commands: Commands,
    mut beavers: Query<(Entity, &mut Beaver, &Transform)>,
    mut jobs: Query<(Entity, &mut Job)>,
    map: Res<Map>,
    buildings: Query<(&Building, Has<UnderConstruction>)>,
) {
    for (beaver_entity, mut beaver, transform) in &mut beavers {
        if beaver.state != BeaverState::Idle {
            continue;
        }
        // Nearest unclaimed job, with construction first.
        let mut best: Option<(Entity, f32, bool)> = None;
        for (job_entity, job) in &jobs {
            if job.claimed_by.is_some() {
                continue;
            }
            let is_build = matches!(job.kind, JobKind::Build(_));
            let pos = map.tile_center(job.tile.x, job.tile.y);
            let dist = pos.distance_squared(transform.translation);
            let better = match best {
                None => true,
                Some((_, best_dist, best_build)) => match (is_build, best_build) {
                    (true, false) => true,
                    (false, true) => false,
                    _ => dist < best_dist,
                },
            };
            if better {
                best = Some((job_entity, dist, is_build));
            }
        }
        if let Some((job_entity, _, _)) = best
            && let Ok((_, mut job)) = jobs.get_mut(job_entity)
        {
            job.claimed_by = Some(beaver_entity);
            beaver.state = BeaverState::Goto(job_entity);
            // Route on the task pool; replacing the slot cancels any
            // stale in-flight search. Until (or unless) a route lands,
            // movement falls back to a straight line.
            if let Some(start) = map.tile_at(transform.translation) {
                let grid = walk_grid(&map, &buildings);
                let goal = job.tile;
                commands.entity(beaver_entity).insert((
                    AsyncValue::<ComputedPath>::Pending,
                    AsyncSlot::new(async move {
                        ComputedPath {
                            job: job_entity,
                            tiles: pathfind::find_path(&grid, start, goal),
                        }
                    }),
                ));
            }
        }
    }
}

type BeaverMovement = (
    &'static mut Beaver,
    &'static mut Transform,
    Option<&'static mut AsyncValue<ComputedPath>>,
);

fn move_beavers(
    time: Res<Time>,
    map: Res<Map>,
    mut beavers: Query<BeaverMovement>,
    jobs: Query<&Job>,
    buildings: Query<&Building, Without<UnderConstruction>>,
) {
    let dt = time.delta_secs();
    for (mut beaver, mut transform, route) in &mut beavers {
        let BeaverState::Goto(job_entity) = beaver.state else {
            continue;
        };
        let Ok(job) = jobs.get(job_entity) else {
            beaver.state = BeaverState::Idle;
            continue;
        };
        let target = map.tile_center(job.tile.x, job.tile.y);
        let to_target = (target - transform.translation).with_y(0.0);
        let dist = to_target.length();
        if dist < 0.25 {
            beaver.state = BeaverState::Work(job_entity);
            continue;
        }
        // Steer toward the next waypoint of the async route when one has
        // landed (consuming waypoints as they are reached); otherwise —
        // still pending, computed for an older job, or unreachable — fall
        // back to the straight line.
        let mut waypoint = target;
        if let Some(mut route) = route
            && let AsyncValue::Ready(computed) = &mut *route
            && computed.job == job_entity
            && let Some(tiles) = &mut computed.tiles
        {
            while let Some(&next) = tiles.first() {
                let center = map.tile_center(next.x, next.y);
                if (center - transform.translation).with_y(0.0).length() < 0.2 {
                    tiles.remove(0);
                } else {
                    waypoint = center;
                    break;
                }
            }
        }
        let to_waypoint = (waypoint - transform.translation).with_y(0.0);
        // Finished paths under the beaver's feet speed it up considerably.
        let current_tile = map.tile_at(transform.translation);
        let on_path = current_tile.is_some_and(|t| {
            map.building[map.idx(t.x, t.y)]
                .and_then(|e| buildings.get(e).ok())
                .is_some_and(|b| b.kind == super::buildings::BuildingKind::Path)
        });
        let speed = if on_path {
            WALK_SPEED * 1.8
        } else {
            WALK_SPEED
        };
        let step = to_waypoint.normalize_or_zero() * speed * dt;
        transform.translation += step.clamp_length_max(to_waypoint.length().min(dist));
        // Stick to the terrain surface and face the walking direction.
        if let Some(tile) = current_tile {
            let ground = map.tile_center(tile.x, tile.y).y;
            let water = map.water[map.idx(tile.x, tile.y)] * super::map::LEVEL;
            transform.translation.y = ground + water;
        }
        if step.length_squared() > 0.0 {
            let yaw = (-step.z).atan2(step.x) + std::f32::consts::FRAC_PI_2;
            transform.rotation = Quat::from_rotation_y(yaw);
        }
    }
}

fn work_jobs(
    mut commands: Commands,
    time: Res<Time>,
    mut map: ResMut<Map>,
    mut stockpile: ResMut<Stockpile>,
    mut beavers: Query<&mut Beaver>,
    mut jobs: Query<&mut Job>,
    trees: Query<&Tree>,
) {
    let dt = time.delta_secs();
    for mut beaver in &mut beavers {
        let BeaverState::Work(job_entity) = beaver.state else {
            continue;
        };
        let Ok(mut job) = jobs.get_mut(job_entity) else {
            beaver.state = BeaverState::Idle;
            continue;
        };
        job.work -= dt;
        if let JobKind::Build(building) = job.kind {
            // Mirror progress onto the building so the UI can show it.
            if let Ok(mut e) = commands.get_entity(building) {
                e.queue(move |mut entity: EntityWorldMut| {
                    if let Some(mut uc) = entity.get_mut::<UnderConstruction>() {
                        uc.done += dt;
                    }
                });
            }
        }
        if job.work > 0.0 {
            continue;
        }

        // Job finished: apply its effect.
        match job.kind {
            JobKind::Build(building) => {
                commands.entity(building).remove::<UnderConstruction>();
            }
            JobKind::Chop(tree_entity) => {
                if let Ok(tree) = trees.get(tree_entity) {
                    let i = map.idx(tree.tile.x, tree.tile.y);
                    // Two lumberjacks can target the same tree; the map slot
                    // is the claim — only the first completer fells it (and
                    // is paid for it).
                    if map.tree[i].is_some() {
                        map.tree[i] = None;
                        commands.entity(tree_entity).despawn();
                        stockpile.logs += 3.0;
                    }
                }
            }
            JobKind::Pump(_) => {
                // Take the water out of the world.
                let tile = job.tile;
                let wet = map
                    .neighbors4(tile.x, tile.y)
                    .find(|n| map.has_water(n.x, n.y));
                if let Some(wet) = wet {
                    let i = map.idx(wet.x, wet.y);
                    map.water[i] = (map.water[i] - 0.15).max(0.0);
                    stockpile.water += 2.0;
                }
            }
            JobKind::Farm(_) => {
                stockpile.food += 2.0;
            }
            JobKind::Plant(tile) => {
                if map.is_free_land(tile.x, tile.y) {
                    spawn_tree(&mut commands, &mut map, tile, 0.0);
                }
            }
        }
        commands.entity(job_entity).despawn();
        beaver.state = BeaverState::Idle;
    }
}

fn needs(
    mut commands: Commands,
    time: Res<Time>,
    mut stockpile: ResMut<Stockpile>,
    mut starving_count: ResMut<StarvingCount>,
    mut notice: ResMut<crate::interact::Notice>,
    real_time: Res<Time<Real>>,
    mut beavers: Query<(Entity, &mut Beaver)>,
) {
    let dt = time.delta_secs();
    for (entity, mut beaver) in &mut beavers {
        beaver.hunger += HUNGER_RATE * dt;
        beaver.thirst += THIRST_RATE * dt;
        let mut deprived = false;
        if beaver.hunger >= 1.0 {
            if stockpile.food >= 1.0 {
                stockpile.food -= 1.0;
                beaver.hunger = 0.0;
            } else {
                beaver.hunger = 1.0;
                deprived = true;
            }
        }
        if beaver.thirst >= 1.0 {
            if stockpile.water >= 1.0 {
                stockpile.water -= 1.0;
                beaver.thirst = 0.0;
            } else {
                beaver.thirst = 1.0;
                deprived = true;
            }
        }
        if deprived {
            if beaver.starving == 0.0 {
                commands.entity(entity).insert(Starving);
                starving_count.0 += 1;
            }
            beaver.starving += dt;
            if beaver.starving > STARVE_LIMIT {
                commands.entity(entity).despawn();
                starving_count.0 = starving_count.0.saturating_sub(1);
                notice.message = Some("A beaver has starved".into());
                notice.expires = real_time.elapsed_secs() + 2.5;
            }
        } else if beaver.starving > 0.0 {
            beaver.starving = 0.0;
            commands.entity(entity).remove::<Starving>();
            starving_count.0 = starving_count.0.saturating_sub(1);
        }
    }
}

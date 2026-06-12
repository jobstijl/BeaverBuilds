//! The cinematic intro: the game opens as a letterboxed attract mode where
//! a rule-based governor plays a colony at speed while the camera glides
//! between points of interest. Any click (or Space/Enter) tears the demo
//! world down and starts a fresh colony — new map seed, full HUD, controls
//! handed to the player.

use bevy::prelude::*;
use bevy_reactive_bsn::AsyncValue;

use crate::AppState;
use crate::chronicle::Chronicle;
use crate::interact::{Hover, Selected, Tool};
use crate::render::camera::CameraRig;
use crate::sim::beavers::{Beaver, Job};
use crate::sim::buildings::{
    Building, BuildingKind, UnderConstruction, def, place_building, placement_error,
};
use crate::sim::map::Map;
use crate::sim::trees::Tree;
use crate::sim::{Population, Season, Stockpile};

pub struct IntroPlugin;

impl Plugin for IntroPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Governor>()
            .init_resource::<Cinematic>()
            .add_systems(Startup, (spawn_overlay, intro_speed))
            .add_systems(
                Update,
                (
                    govern,
                    cinematic_camera,
                    start_on_input,
                    restart_demo_if_fallen,
                )
                    .run_if(in_state(AppState::Intro)),
            )
            .add_systems(
                Update,
                pulse_prompt.run_if(not(in_state(AppState::Playing))),
            )
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    despawn_overlay,
                    reset_world,
                    crate::render::spawn_terrain,
                    crate::sim::trees::scatter_initial_trees,
                    crate::sim::beavers::initial_colony,
                )
                    .chain()
                    // Only on RE-entry (intro click, game-over restart):
                    // when booting straight into Playing (BB_SKIP_INTRO),
                    // this initial OnEnter fires before PreStartup has even
                    // loaded assets — Startup builds the first world.
                    .run_if(resource_exists::<crate::render::TileEntities>),
            )
            .add_systems(OnEnter(AppState::GameOver), spawn_epitaph)
            .add_systems(
                Update,
                restart_on_input.run_if(in_state(AppState::GameOver)),
            );
    }
}

// ---------------------------------------------------------------------------
// Game over: the epitaph, and the road back
// ---------------------------------------------------------------------------

fn spawn_epitaph(mut commands: Commands, season: Res<Season>, stats: Res<crate::sim::ColonyStats>) {
    let line = format!(
        "The colony fell on day {} — {} beavers at its height, {} droughts endured",
        season.day,
        stats.peak,
        season.cycle.saturating_sub(u32::from(!season.drought))
    );
    commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                top: px(0),
                left: px(0),
                right: px(0),
                bottom: px(0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: px(12),
            }
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55))
            Children [
                (
                    Text("THE COLONY FELL")
                    TextFont { font_size: px(52) }
                    TextColor(Color::srgb(1.0, 0.45, 0.35))
                    TextShadow
                ),
                (
                    Text({ line })
                    TextFont { font_size: px(16) }
                    TextColor(Color::srgba(1.0, 1.0, 1.0, 0.8))
                ),
                (
                    template_value(Prompt)
                    Text("click to found a new colony")
                    TextFont { font_size: px(18) }
                    TextColor(Color::srgb(1.0, 0.92, 0.75))
                ),
            ]
        })
        .insert(IntroOverlay);
}

fn restart_on_input(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut next: ResMut<NextState<AppState>>,
) {
    if mouse.just_pressed(MouseButton::Left)
        || keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::Enter)
    {
        info!(target: "player", "restart after game over");
        next.set(AppState::Playing);
    }
}

fn intro_speed(state: Res<State<AppState>>, mut time: ResMut<Time<Virtual>>) {
    // The intro runs hot so things visibly happen; BB_FAST still overrides.
    if *state.get() == AppState::Intro && std::env::var("BB_FAST").is_err() {
        time.set_relative_speed(5.0);
    }
}

// ---------------------------------------------------------------------------
// Start transition
// ---------------------------------------------------------------------------

fn start_on_input(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time<Real>>,
    mut next: ResMut<NextState<AppState>>,
) {
    // BB_INTRO_SECS=n auto-starts after n seconds (agent/CI verification).
    let auto = std::env::var("BB_INTRO_SECS")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .is_some_and(|secs| time.elapsed_secs() > secs);
    if auto
        || mouse.just_pressed(MouseButton::Left)
        || keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::Enter)
    {
        info!(target: "player", "intro ended ({})", if auto { "auto" } else { "input" });
        next.set(AppState::Playing);
    }
}

/// Tear the demo world down and prepare a fresh one (the spawn systems run
/// right after this in the same `OnEnter` chain).
fn reset_world(world: &mut World) {
    let mut doomed: Vec<Entity> = Vec::new();
    if let Some(tiles) = world.get_resource::<crate::render::TileEntities>() {
        doomed.extend(tiles.ground.iter().copied());
        doomed.extend(tiles.water.iter().copied());
    }
    macro_rules! collect {
        ($t:ty) => {
            doomed.extend(
                world
                    .query_filtered::<Entity, With<$t>>()
                    .iter(world)
                    .collect::<Vec<_>>(),
            );
        };
    }
    collect!(Tree);
    collect!(Building);
    collect!(Beaver);
    collect!(Job);
    for entity in doomed {
        if let Ok(e) = world.get_entity_mut(entity) {
            e.despawn();
        }
    }

    // A fresh map seed for the player's run (the intro used the default).
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_millis() % 1000) as f32)
        .unwrap_or(0.0);
    world.insert_resource(Map::generate_seeded(48, 48, seed));
    world.insert_resource(Stockpile {
        logs: 30.0,
        food: 12.0,
        water: 12.0,
    });
    world.insert_resource(Season::default());
    world.insert_resource(Population::default());
    world.insert_resource(crate::sim::ColonyStats::default());
    world.resource_mut::<Chronicle>().0.clear();
    world.resource_mut::<Selected>().0 = None;
    world.resource_mut::<Hover>().0 = None;
    *world.resource_mut::<Tool>() = Tool::Select;
    *world.resource_mut::<CameraRig>() = CameraRig::default();
    let mut time = world.resource_mut::<Time<Virtual>>();
    time.unpause();
    if std::env::var("BB_FAST").is_err() {
        time.set_relative_speed(1.0);
    }
}

/// If the demo colony falls, the attract mode lingers on the ruins for a
/// beat and then quietly starts over on a fresh seed. The title screen
/// never exits to the game-over screen — that one is for players — and
/// never sits on a dead world forever. `Local` state: (colony was alive,
/// seconds since it fell).
fn restart_demo_if_fallen(world: &mut World, mut state: Local<(bool, f32)>) {
    let population = world.resource::<Population>().count;
    if population > 0 {
        *state = (true, 0.0);
        return;
    }
    if !state.0 {
        return;
    }
    state.1 += world.resource::<Time<Real>>().delta_secs();
    if state.1 < 6.0 {
        return;
    }
    *state = (false, 0.0);
    info!("intro governor: the demo colony fell — restarting the attract mode");
    reset_world(world);
    let _ = world.run_system_cached(crate::render::spawn_terrain);
    let _ = world.run_system_cached(crate::sim::trees::scatter_initial_trees);
    let _ = world.run_system_cached(crate::sim::beavers::initial_colony);
    *world.resource_mut::<Governor>() = Governor::default();
    *world.resource_mut::<Cinematic>() = Cinematic::default();
    // reset_world sets player speed; the attract mode runs hot.
    if std::env::var("BB_FAST").is_err() {
        world
            .resource_mut::<Time<Virtual>>()
            .set_relative_speed(5.0);
    }
}

// ---------------------------------------------------------------------------
// The governor (the colony plays itself during the intro)
// ---------------------------------------------------------------------------

#[derive(Resource, Default)]
pub(crate) struct Governor {
    cooldown: f32,
    /// Pending road: tiles still to pave, one per beat.
    road: Vec<UVec2>,
    /// A planned dam crossing: the full wet width of the river at a chosen
    /// narrow point, built tile by tile like a real construction project.
    dam_project: Vec<UVec2>,
    /// The most recent placement — a camera point of interest.
    last_built: Option<UVec2>,
}

/// One action every few seconds, survival first: water and food before
/// growth, growth before drought-proofing. Skips a beat when it can't
/// afford its choice, falling back to a cheap lumberjack rather than
/// idling into a death spiral.
#[allow(clippy::too_many_arguments)]
pub(crate) fn govern(
    mut commands: Commands,
    time: Res<Time>,
    mut governor: ResMut<Governor>,
    mut map: ResMut<Map>,
    mut stockpile: ResMut<Stockpile>,
    population: Res<Population>,
    buildings: Query<(&Building, Has<UnderConstruction>)>,
    constructing: Query<(), With<UnderConstruction>>,
    trees: Query<&Tree>,
    forecast: Query<&AsyncValue<f32>>,
) {
    governor.cooldown -= time.delta_secs();
    if governor.cooldown > 0.0 {
        return;
    }
    governor.cooldown = 2.0;

    // Roads grow every beat, alongside whatever else is happening.
    if let Some(tile) = governor.road.pop()
        && stockpile.logs >= def(BuildingKind::Path).cost_logs
        && placement_error(&map, BuildingKind::Path, tile).is_none()
    {
        place_building(
            &mut commands,
            &mut map,
            &mut stockpile,
            BuildingKind::Path,
            tile,
        );
    }

    if constructing.iter().count() >= 2 {
        return;
    }

    let count = |kind: BuildingKind| buildings.iter().filter(|(b, _)| b.kind == kind).count();
    let anchor = colony_anchor(&map, &buildings);
    let retention = forecast.iter().find_map(|v| v.ready().copied());

    // Dam first, like the strategy guide says: plan the crossing as soon as
    // the first lumberjack provides income, build it before anything fancy.
    if governor.dam_project.is_empty()
        && count(BuildingKind::Dam) == 0
        && count(BuildingKind::Lumberjack) >= 1
    {
        governor.dam_project = plan_dam_crossing(&map, anchor, &[]);
        if !governor.dam_project.is_empty() {
            info!(
                "intro governor: planned a {}-tile dam at {:?}",
                governor.dam_project.len(),
                governor.dam_project
            );
        }
    }
    // Once the colony stands behind its first wall, keep building: a second
    // crossing extends the reservoir, which is exactly what the escalating
    // droughts demand (and it keeps the attract mode visually alive).
    if governor.dam_project.is_empty()
        && count(BuildingKind::Dam) >= 2
        && count(BuildingKind::Dam) < 8
        && population.count >= 6
        && stockpile.logs > 35.0
    {
        let existing: Vec<UVec2> = buildings
            .iter()
            .filter(|(b, _)| b.kind == BuildingKind::Dam)
            .map(|(b, _)| b.tile)
            .collect();
        governor.dam_project = plan_dam_crossing(&map, anchor, &existing);
        if !governor.dam_project.is_empty() {
            info!(
                "intro governor: expanding the reservoir — new wall at {:?}",
                governor.dam_project
            );
        }
    }
    // An unfinished wall takes precedence over growth — but never over
    // having an income (cheap lumberjack fallback) or the bootstrap trio.
    if let Some(&tile) = governor.dam_project.last()
        && count(BuildingKind::WaterPump) >= 1
        && count(BuildingKind::CarrotFarm) >= 1
    {
        if stockpile.logs >= def(BuildingKind::Dam).cost_logs {
            if placement_error(&map, BuildingKind::Dam, tile).is_none() {
                governor.dam_project.pop();
                place_building(
                    &mut commands,
                    &mut map,
                    &mut stockpile,
                    BuildingKind::Dam,
                    tile,
                );
                governor.last_built = Some(tile);
                info!("intro governor: dam wall segment at {tile}");
            } else {
                // A partial wall is useless: abandon the blocked project.
                warn!("intro governor: dam segment at {tile} blocked; abandoning project");
                governor.dam_project.clear();
            }
        } else if count(BuildingKind::Lumberjack) < 2 + population.count as usize / 3
            && stockpile.logs >= def(BuildingKind::Lumberjack).cost_logs
            && let Some(spot) = find_spot(&map, BuildingKind::Lumberjack, anchor, &trees, None)
        {
            place_building(
                &mut commands,
                &mut map,
                &mut stockpile,
                BuildingKind::Lumberjack,
                spot,
            );
        }
        return;
    }

    let choice = if count(BuildingKind::Lumberjack) == 0 {
        Some(BuildingKind::Lumberjack)
    } else if count(BuildingKind::WaterPump) == 0 {
        Some(BuildingKind::WaterPump)
    } else if count(BuildingKind::CarrotFarm) == 0 {
        Some(BuildingKind::CarrotFarm)
    } else if count(BuildingKind::Forester) == 0 {
        // The forester is part of the bootstrap, not a luxury: three flags
        // chop the colony bare within days, and replanting must start
        // before the last mature tree falls — afterwards there is no wood
        // income left to pay for the fix.
        Some(BuildingKind::Forester)
    } else if stockpile.water < 8.0 + population.count as f32
        && count(BuildingKind::WaterPump) < 1 + population.count as usize / 3
    {
        Some(BuildingKind::WaterPump)
    } else if stockpile.food < 8.0 + population.count as f32
        && count(BuildingKind::CarrotFarm) < 1 + population.count as usize / 4
    {
        Some(BuildingKind::CarrotFarm)
    } else if stockpile.logs < 25.0
        && count(BuildingKind::Lumberjack) < 2 + population.count as usize / 3
    {
        // Wood is the master resource: every other plan stalls without it.
        // Flags scale with population and chase the remaining mature trees
        // (find_spot requires one in range), so a chopped-out radius never
        // freezes the treasury again.
        Some(BuildingKind::Lumberjack)
    } else if count(BuildingKind::Forester) < 1 + population.count as usize / 6
        && trees.iter().count() < 240
    {
        Some(BuildingKind::Forester)
    } else if population.count >= population.cap && count(BuildingKind::Dam) >= 2 {
        // Growth only behind a finished wall: every birth is a mouth that
        // must outlive the next drought.
        Some(BuildingKind::Lodge)
    } else if stockpile.logs > 35.0 && count(BuildingKind::Dam) >= 2 {
        Some(BuildingKind::Lodge)
    } else if stockpile.logs > 28.0 {
        // Prosperity building: spend the surplus ahead of demand. More
        // capacity means the next growth spurt doesn't dip the stocks, and
        // the attract mode never sits idle.
        Some(
            if count(BuildingKind::WaterPump) <= count(BuildingKind::CarrotFarm) {
                BuildingKind::WaterPump
            } else {
                BuildingKind::CarrotFarm
            },
        )
    } else {
        None
    };
    let _ = retention;

    if let Some(mut kind) = choice {
        if stockpile.logs < def(kind).cost_logs {
            if count(BuildingKind::Lumberjack) < 3
                && stockpile.logs >= def(BuildingKind::Lumberjack).cost_logs
            {
                kind = BuildingKind::Lumberjack;
            } else {
                return; // save up; try again next beat
            }
        }
        // Pumps aim for the *pool side* of the wall: the midpoint between
        // colony and dam lies on the future reservoir. Hugging the dam tile
        // itself is a coin flip between shores — and the downstream shore
        // goes permanently dry the moment the wall holds.
        let near = (kind == BuildingKind::WaterPump)
            .then(|| {
                governor
                    .dam_project
                    .first()
                    .copied()
                    .or_else(|| {
                        buildings
                            .iter()
                            .find(|(b, _)| b.kind == BuildingKind::Dam)
                            .map(|(b, _)| b.tile)
                    })
                    .map(|dam| (anchor + dam) / 2)
            })
            .flatten();
        if let Some(tile) = find_spot(&map, kind, anchor, &trees, near) {
            place_building(&mut commands, &mut map, &mut stockpile, kind, tile);
            governor.last_built = Some(tile);
            info!("intro governor: building {} at {tile}", def(kind).name);
            if matches!(
                kind,
                BuildingKind::Lumberjack
                    | BuildingKind::WaterPump
                    | BuildingKind::CarrotFarm
                    | BuildingKind::Lodge
            ) {
                // Route the road like the beavers will walk it: A* that
                // prefers existing paths, so roads merge into a network.
                let grid = crate::sim::beavers::walk_grid(&map, &buildings);
                if let Some(route) = crate::sim::pathfind::find_path(&grid, anchor, tile) {
                    let mut road: Vec<UVec2> = route;
                    road.pop(); // not the building tile itself
                    road.reverse(); // pave outward from the colony
                    governor.road = road;
                }
            }
        }
    }
}

/// Pick the narrowest river section in a band downstream of the colony and
/// return its wet tiles (the dam wall), nearest-to-colony first. `avoid`
/// lists existing dam tiles; new crossings keep at least 6 tiles of river
/// between walls so each one impounds a real stretch of water.
fn plan_dam_crossing(map: &Map, anchor: UVec2, avoid: &[UVec2]) -> Vec<UVec2> {
    let mut best: Option<Vec<UVec2>> = None;
    for dx in 4..(map.width as i32 / 2) {
        for x in [anchor.x as i32 + dx, anchor.x as i32 - dx] {
            if x < 0 || x as u32 >= map.width {
                continue;
            }
            if avoid.iter().any(|d| (d.x as i32 - x).abs() < 6) {
                continue;
            }
            let wall: Vec<UVec2> = (0..map.height)
                .map(|y| UVec2::new(x as u32, y))
                .filter(|t| map.is_river_bed(t.x, t.y))
                .filter(|t| placement_error(map, BuildingKind::Dam, *t).is_none())
                .collect();
            if wall.is_empty() || wall.len() > 3 {
                continue; // no river here, or too wide to seal
            }
            // Contiguous?
            let contiguous = wall.windows(2).all(|w| w[1].y - w[0].y == 1);
            if contiguous && best.as_ref().is_none_or(|b| wall.len() < b.len()) {
                best = Some(wall);
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.unwrap_or_default()
}

/// Where the colony "is": the first lodge, else the map center.
fn colony_anchor(map: &Map, buildings: &Query<(&Building, Has<UnderConstruction>)>) -> UVec2 {
    buildings
        .iter()
        .find(|(b, _)| b.kind == BuildingKind::Lodge)
        .map(|(b, _)| b.tile)
        .unwrap_or(UVec2::new(map.width / 2, map.height / 2))
}

/// Nearest valid tile to `anchor` (or to `near` when given — e.g. pumps
/// hugging the future reservoir), with kind-specific preferences.
fn find_spot(
    map: &Map,
    kind: BuildingKind,
    anchor: UVec2,
    trees: &Query<&Tree>,
    near: Option<UVec2>,
) -> Option<UVec2> {
    let anchor = near.unwrap_or(anchor);
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

// ---------------------------------------------------------------------------
// Cinematic camera: glide between points of interest
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Pose {
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

#[derive(Resource)]
struct Cinematic {
    t: f32,
    duration: f32,
    from: Pose,
    to: Pose,
    shot: usize,
}

impl Default for Cinematic {
    fn default() -> Self {
        let start = Pose {
            focus: Vec3::ZERO,
            yaw: 0.6,
            pitch: 0.9,
            distance: 30.0,
        };
        Self {
            t: 0.0,
            duration: 0.01, // pick a real shot immediately
            from: start,
            to: start,
            shot: 0,
        }
    }
}

fn cinematic_camera(
    time: Res<Time<Real>>,
    mut cine: ResMut<Cinematic>,
    mut rig: ResMut<CameraRig>,
    map: Res<Map>,
    governor: Res<Governor>,
    buildings: Query<&Building>,
) {
    cine.t += time.delta_secs();
    if cine.t >= cine.duration {
        cine.t = 0.0;
        cine.duration = 9.0;
        cine.from = cine.to;
        cine.shot += 1;
        // Points of interest, in rotation: the newest building, the colony,
        // a dam (if any), the river inlet, a wide establishing view.
        let center = UVec2::new(map.width / 2, map.height / 2);
        let poi = match cine.shot % 4 {
            0 => governor.last_built.unwrap_or(center),
            1 => buildings
                .iter()
                .find(|b| b.kind == BuildingKind::Dam)
                .map(|b| b.tile)
                .unwrap_or(center),
            2 => (0..map.height)
                .flat_map(|y| (0..map.width).map(move |x| UVec2::new(x, y)))
                .find(|t| map.source[map.idx(t.x, t.y)])
                .unwrap_or(center),
            _ => center,
        };
        let wide = cine.shot % 4 == 3;
        cine.to = Pose {
            focus: map.tile_center(poi.x, poi.y),
            yaw: cine.from.yaw + 1.1,
            pitch: if wide { 1.05 } else { 0.65 },
            distance: if wide { 30.0 } else { 12.0 },
        };
    }
    let s = (cine.t / cine.duration).clamp(0.0, 1.0);
    let s = s * s * (3.0 - 2.0 * s); // smoothstep
    let drift = 0.04 * cine.t; // gentle pan inside the shot
    rig.focus = cine.from.focus.lerp(cine.to.focus, s);
    rig.yaw = cine.from.yaw + (cine.to.yaw - cine.from.yaw) * s + drift;
    rig.pitch = cine.from.pitch + (cine.to.pitch - cine.from.pitch) * s;
    rig.distance = cine.from.distance + (cine.to.distance - cine.from.distance) * s;
}

// ---------------------------------------------------------------------------
// Overlay: letterbox + title + pulsing prompt
// ---------------------------------------------------------------------------

#[derive(Component)]
struct IntroOverlay;

#[derive(Component, Clone, Default)]
struct Prompt;

fn spawn_overlay(mut commands: Commands, state: Res<State<AppState>>) {
    if *state.get() != AppState::Intro {
        return;
    }
    commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                top: px(0),
                left: px(0),
                right: px(0),
                bottom: px(0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
            }
            Children [
                (
                    Node {
                        width: percent(100),
                        height: px(70),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::FlexEnd,
                    }
                    BackgroundColor(Color::BLACK)
                ),
                (
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: px(10),
                    }
                    Children [
                        (
                            Text("BEAVERBUILDS")
                            TextFont { font_size: px(64) }
                            TextColor(Color::srgb(1.0, 0.92, 0.75))
                            TextShadow
                        ),
                        (
                            Text("a colony plays itself while you watch")
                            TextFont { font_size: px(16) }
                            TextColor(Color::srgba(1.0, 1.0, 1.0, 0.7))
                        ),
                    ]
                ),
                (
                    Node {
                        width: percent(100),
                        height: px(70),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                    }
                    BackgroundColor(Color::BLACK)
                    Children [(
                        template_value(Prompt)
                        Text("click to take command")
                        TextFont { font_size: px(20) }
                        TextColor(Color::srgb(1.0, 0.92, 0.75))
                    )]
                ),
            ]
        })
        .insert(IntroOverlay);
}

fn pulse_prompt(time: Res<Time<Real>>, mut prompts: Query<&mut TextColor, With<Prompt>>) {
    let alpha = 0.45 + 0.55 * (time.elapsed_secs() * 2.2).sin().abs();
    for mut color in &mut prompts {
        color.0 = color.0.with_alpha(alpha);
    }
}

fn despawn_overlay(mut commands: Commands, overlays: Query<Entity, With<IntroOverlay>>) {
    for overlay in &overlays {
        commands.entity(overlay).despawn();
    }
}

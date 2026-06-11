pub mod beavers;
pub mod buildings;
pub mod map;
pub mod pathfind;
pub mod trees;
pub mod water;

use bevy::prelude::*;

/// Day length and season lengths, in (virtual) seconds.
pub const DAY_LENGTH: f32 = 30.0;
pub const WET_LENGTH: f32 = 90.0;
pub const DROUGHT_LENGTH: f32 = 30.0;

/// Global stockpile of the colony. Kept global (rather than per-warehouse)
/// to keep the prototype focused; jobs still require beavers to physically
/// walk to the work site.
#[derive(Resource, Default)]
pub struct Stockpile {
    pub logs: f32,
    pub food: f32,
    pub water: f32,
}

#[derive(Resource)]
pub struct Season {
    pub drought: bool,
    /// Seconds remaining in the current season.
    pub remaining: f32,
    pub day: u32,
    day_timer: f32,
}

/// Bumped every few seconds: the throttle for expensive derived state like
/// the async drought forecast (which also reacts to dam construction).
#[derive(Resource, Default)]
pub struct ForecastTick(pub u32);

impl Default for Season {
    fn default() -> Self {
        Self {
            drought: false,
            remaining: WET_LENGTH,
            day: 1,
            day_timer: 0.0,
        }
    }
}

#[derive(Resource, Default)]
pub struct Population {
    pub count: u32,
    pub cap: u32,
}

pub struct SimPlugin;

impl Plugin for SimPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Stockpile {
            logs: 30.0,
            food: 12.0,
            water: 12.0,
        })
        .init_resource::<Season>()
        .init_resource::<ForecastTick>()
        .init_resource::<Population>()
        .insert_resource(Time::<Fixed>::from_hz(16.0))
        .insert_resource(map::Map::generate(48, 48))
        .add_systems(FixedUpdate, (advance_season, bump_forecast_tick))
        .add_plugins((
            water::WaterPlugin,
            trees::TreesPlugin,
            buildings::BuildingsPlugin,
            beavers::BeaversPlugin,
        ))
        .add_systems(Update, game_speed_hotkeys);
    }
}

fn advance_season(time: Res<Time>, mut season: ResMut<Season>) {
    // Written naturally every tick: UI that wants coarser wake granularity
    // declares it at the dependency (`Dep::resource_value`), not here.
    let dt = time.delta_secs();
    season.remaining -= dt;
    if season.remaining <= 0.0 {
        season.drought = !season.drought;
        season.remaining = if season.drought {
            DROUGHT_LENGTH
        } else {
            WET_LENGTH
        };
    }
    season.day_timer += dt;
    if season.day_timer >= DAY_LENGTH {
        season.day_timer -= DAY_LENGTH;
        season.day += 1;
    }
}

fn bump_forecast_tick(
    time: Res<Time>,
    mut accumulated: Local<f32>,
    mut tick: ResMut<ForecastTick>,
) {
    *accumulated += time.delta_secs();
    if *accumulated >= 5.0 {
        *accumulated = 0.0;
        tick.0 = tick.0.wrapping_add(1);
    }
}

/// Space pauses, 1/2/3 set game speed.
fn game_speed_hotkeys(keys: Res<ButtonInput<KeyCode>>, mut time: ResMut<Time<Virtual>>) {
    if keys.just_pressed(KeyCode::Space) {
        if time.is_paused() {
            time.unpause();
        } else {
            time.pause();
        }
    }
    if keys.just_pressed(KeyCode::Digit1) {
        time.set_relative_speed(1.0);
        time.unpause();
    }
    if keys.just_pressed(KeyCode::Digit2) {
        time.set_relative_speed(2.0);
        time.unpause();
    }
    if keys.just_pressed(KeyCode::Digit3) {
        time.set_relative_speed(4.0);
        time.unpause();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;

    use super::buildings::{BuildingKind, UnderConstruction, place_building};
    use super::trees::Tree;
    use super::*;

    /// Headless, deterministic-time end-to-end run: place a lumberjack near
    /// mature trees, advance ~45 simulated seconds, and require the whole
    /// chain to have worked — construction finished, trees chopped (through
    /// async pathfinding), logs in the stockpile.
    #[test]
    fn economy_runs_headless() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                62,
            )))
            // No input plugin headless; the speed-hotkey system wants it.
            .init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(bevy_reactive_bsn::ReactiveBsnPlugin)
            .add_plugins(SimPlugin);
        app.update(); // Startup: map, trees, the initial colony.

        let world = app.world_mut();
        let spot = {
            let mature: Vec<UVec2> = world
                .query::<&Tree>()
                .iter(world)
                .filter(|t| t.is_mature())
                .map(|t| t.tile)
                .collect();
            assert!(!mature.is_empty(), "initial scatter has mature trees");
            let map = world.resource::<map::Map>();
            let center = UVec2::new(map.width / 2, map.height / 2).as_ivec2();
            let mut mature = mature;
            mature.sort_by_key(|t| (t.as_ivec2() - center).length_squared());
            mature
                .iter()
                .find_map(|tree| {
                    let r = 4i32;
                    (-r..=r)
                        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
                        .find_map(|(dx, dy)| {
                            let (x, y) = (tree.x as i32 + dx, tree.y as i32 + dy);
                            (map.in_bounds(x, y) && map.is_free_land(x as u32, y as u32))
                                .then(|| UVec2::new(x as u32, y as u32))
                        })
                })
                .expect("free land near a mature tree")
        };
        let trees_before = world.query::<&Tree>().iter(world).count();
        world.resource_scope(|world, mut map: Mut<map::Map>| {
            world.resource_scope(|world, mut stockpile: Mut<Stockpile>| {
                let mut commands = world.commands();
                place_building(
                    &mut commands,
                    &mut map,
                    &mut stockpile,
                    BuildingKind::Lumberjack,
                    spot,
                );
            });
        });
        world.flush();
        let logs_after_placement = world.resource::<Stockpile>().logs;

        for _ in 0..900 {
            app.update(); // ~56 simulated seconds at 16 Hz.
        }

        let world = app.world_mut();
        assert_eq!(
            world.query::<&UnderConstruction>().iter(world).count(),
            0,
            "construction must finish"
        );
        let trees_after = world.query::<&Tree>().iter(world).count();
        assert!(
            trees_after < trees_before,
            "beavers must have chopped trees ({trees_before} -> {trees_after})"
        );
        assert!(
            world.resource::<Stockpile>().logs > logs_after_placement,
            "chopping must yield logs"
        );
    }
}

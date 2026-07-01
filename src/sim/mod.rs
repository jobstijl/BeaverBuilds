pub mod beavers;
pub mod buildings;
pub mod map;
pub mod pathfind;
pub mod trees;
pub mod water;

use bevy::prelude::*;

/// Day length, in (virtual) seconds.
pub const DAY_LENGTH: f32 = 30.0;

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
    /// How many droughts have come (and gone): each one is longer.
    pub cycle: u32,
    day_timer: f32,
}

impl Season {
    /// Droughts escalate: survive one, and the next bites harder.
    pub fn drought_length(cycle: u32) -> f32 {
        (20.0 + 8.0 * cycle as f32).min(55.0)
    }

    pub fn wet_length(cycle: u32) -> f32 {
        (90.0 - 4.0 * cycle as f32).max(60.0)
    }

    /// The length of the *next* drought — what the forecast simulates.
    pub fn next_drought_length(&self) -> f32 {
        Self::drought_length(self.cycle + u32::from(!self.drought))
    }
}

/// Bumped every few seconds: the throttle for expensive derived state like
/// the async drought forecast (which also reacts to dam construction).
#[derive(Resource, Default)]
pub struct ForecastTick(pub u32);

impl Default for Season {
    fn default() -> Self {
        Self {
            drought: false,
            remaining: Self::wet_length(0),
            day: 1,
            cycle: 0,
            day_timer: 0.0,
        }
    }
}

#[derive(Resource, Default)]
pub struct Population {
    pub count: u32,
    pub cap: u32,
}

/// Colony lifetime stats; also the game-over trigger (a colony that *had*
/// beavers and reaches zero has fallen).
#[derive(Resource, Default)]
pub struct ColonyStats {
    pub started: bool,
    pub peak: u32,
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
        .init_resource::<ColonyStats>()
        .insert_resource(Time::<Fixed>::from_hz(16.0))
        .insert_resource(map::Map::generate(48, 48))
        .add_systems(FixedUpdate, (advance_season, bump_forecast_tick))
        .add_plugins((
            water::WaterPlugin,
            trees::TreesPlugin,
            buildings::BuildingsPlugin,
            beavers::BeaversPlugin,
        ))
        .add_systems(
            Update,
            (
                game_speed_hotkeys.run_if(crate::menu::menu_closed),
                check_colony_fell,
            )
                .run_if(in_state(crate::AppState::Playing)),
        );
    }
}

fn advance_season(time: Res<Time>, mut season: ResMut<Season>) {
    // Written naturally every tick: UI that wants coarser wake granularity
    // declares it at the dependency (`Dep::resource_value`), not here.
    let dt = time.delta_secs();
    season.remaining -= dt;
    if season.remaining <= 0.0 {
        season.drought = !season.drought;
        if season.drought {
            season.cycle += 1;
            season.remaining = Season::drought_length(season.cycle);
        } else {
            season.remaining = Season::wet_length(season.cycle);
        }
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

fn check_colony_fell(
    population: Res<Population>,
    mut stats: ResMut<ColonyStats>,
    mut next: ResMut<NextState<crate::AppState>>,
    mut time: ResMut<Time<Virtual>>,
) {
    if population.count > 0 {
        stats.started = true;
        stats.peak = stats.peak.max(population.count);
    } else if stats.started {
        time.pause();
        next.set(crate::AppState::GameOver);
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

    fn sim_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                62,
            )))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_plugins(bevy::state::app::StatesPlugin)
            .insert_state(crate::AppState::Playing)
            .init_resource::<crate::interact::Notice>()
            .add_plugins(bevy_reactive_bsn::ReactiveBsnPlugin)
            .add_plugins(SimPlugin);
        app
    }

    #[test]
    fn droughts_escalate() {
        assert!(Season::drought_length(2) > Season::drought_length(1));
        assert!(Season::wet_length(3) < Season::wet_length(0));
        let fresh = Season::default();
        assert_eq!(fresh.next_drought_length(), Season::drought_length(1));
    }

    /// Lodges raise beavers, and every birth is paid for in food and water.
    #[test]
    fn lodges_raise_beavers_and_births_cost_resources() {
        let mut app = sim_app();
        app.update();
        let world = app.world_mut();
        // Plenty of supplies; build the lodge instantly for the test.
        world.resource_mut::<Stockpile>().food = 40.0;
        world.resource_mut::<Stockpile>().water = 40.0;
        let spot = {
            let map = world.resource::<map::Map>();
            let center = UVec2::new(map.width / 2, map.height / 2);
            (0..map.height)
                .flat_map(|y| (0..map.width).map(move |x| UVec2::new(x, y)))
                .filter(|t| map.is_free_land(t.x, t.y))
                .min_by_key(|t| t.as_ivec2().distance_squared(center.as_ivec2()))
                .unwrap()
        };
        let lodge = world.resource_scope(|world, mut map: Mut<map::Map>| {
            world.resource_scope(|world, mut stockpile: Mut<Stockpile>| {
                let mut commands = world.commands();
                place_building(
                    &mut commands,
                    &mut map,
                    &mut stockpile,
                    BuildingKind::Lodge,
                    spot,
                )
            })
        });
        world.flush();
        world.entity_mut(lodge).remove::<UnderConstruction>();

        for _ in 0..700 {
            app.update(); // ~43 simulated seconds: at least one nest cycle.
        }
        let world = app.world_mut();
        let population = world.resource::<Population>().count;
        assert!(
            population > 3,
            "the lodge must have raised beavers (population {population})"
        );
        let food = world.resource::<Stockpile>().food;
        assert!(
            food < 40.0 - super::buildings::BIRTH_FOOD,
            "births (and appetites) must consume food: {food}"
        );
    }

    /// A colony that had beavers and lost them all is game over.
    #[test]
    fn colony_collapse_triggers_game_over() {
        let mut app = sim_app();
        for _ in 0..5 {
            app.update(); // colony starts; stats.started flips
        }
        let world = app.world_mut();
        assert!(world.resource::<ColonyStats>().started);
        let beavers: Vec<Entity> = world
            .query_filtered::<Entity, With<beavers::Beaver>>()
            .iter(world)
            .collect();
        assert!(!beavers.is_empty());
        for beaver in beavers {
            world.entity_mut(beaver).despawn();
        }
        for _ in 0..5 {
            app.update();
        }
        assert_eq!(
            *app.world().resource::<State<crate::AppState>>().get(),
            crate::AppState::GameOver,
            "losing every beaver must end the game"
        );
    }

    /// Speed hotkeys must not pierce the pause menu: Space/1/2/3 while the
    /// menu is open would unpause the colony behind a screen saying PAUSED.
    #[test]
    fn speed_hotkeys_do_not_pierce_the_pause_menu() {
        let mut app = sim_app();
        app.init_resource::<crate::menu::MenuState>();
        app.update();

        // The menu is open and has paused the clock; the player presses "2".
        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        app.world_mut()
            .resource_mut::<crate::menu::MenuState>()
            .open = true;
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Digit2);
        app.update();
        assert!(
            app.world().resource::<Time<Virtual>>().is_paused(),
            "a speed hotkey pressed while the menu is open must not unpause"
        );

        // With the menu closed the same key works again. (No InputPlugin in
        // this headless app, so `just_pressed` persists until cleared — the
        // gate above is the run condition, not input consumption.)
        app.world_mut()
            .resource_mut::<crate::menu::MenuState>()
            .open = false;
        app.update();
        assert!(
            !app.world().resource::<Time<Virtual>>().is_paused(),
            "with the menu closed, speed hotkeys apply"
        );
    }

    /// Headless, deterministic-time end-to-end run: place a lumberjack near
    /// mature trees, advance ~45 simulated seconds, and require the whole
    /// chain to have worked — construction finished, trees chopped (through
    /// async pathfinding), logs in the stockpile.
    #[test]
    fn economy_runs_headless() {
        let mut app = sim_app();
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

    /// The attract mode plays this colony unattended on the title screen:
    /// the governor must keep it alive *and visibly growing* through the
    /// first escalating droughts. This pins the whole intro economy — the
    /// pool-side pump placement, the forester bootstrap, the survival job
    /// priority — at once.
    #[test]
    fn intro_governor_survives_and_keeps_building() {
        let mut app = sim_app();
        app.init_resource::<crate::intro::Governor>();
        app.add_systems(Update, crate::intro::govern);
        app.update(); // Startup: map, trees, the initial colony.

        fn buildings(app: &mut App) -> usize {
            let world = app.world_mut();
            world
                .query::<&super::buildings::Building>()
                .iter(world)
                .count()
        }
        let steps_per_day = (DAY_LENGTH / 0.062) as usize;
        for _ in 0..8 * steps_per_day {
            app.update();
        }
        let mid = buildings(&mut app);
        assert!(
            app.world().resource::<Population>().count > 0,
            "demo colony alive at day 8"
        );
        for _ in 0..8 * steps_per_day {
            app.update();
        }
        let end = buildings(&mut app);
        let population = app.world().resource::<Population>().count;
        assert!(
            population > 0,
            "the demo colony must outlive the first droughts"
        );
        assert!(
            end > mid,
            "the governor keeps building ({mid} -> {end} buildings)"
        );
    }
}

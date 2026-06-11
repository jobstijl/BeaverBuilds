pub mod beavers;
pub mod buildings;
pub mod map;
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

/// Seconds remaining in the current season, in whole seconds, written only
/// when the displayed value actually changes. UI that shows the countdown
/// depends on this instead of `Season` (whose `remaining` field ticks every
/// frame), so the text reactor wakes once per second instead of 60×.
#[derive(Resource, Default, PartialEq)]
pub struct SeasonClock(pub u32);

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
        .init_resource::<SeasonClock>()
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

fn advance_season(time: Res<Time>, mut season: ResMut<Season>, mut clock: ResMut<SeasonClock>) {
    let dt = time.delta_secs();
    // The countdown ticks every frame; updating it through
    // `bypass_change_detection` keeps `Season` change-quiet so reactors
    // watching it only wake on real transitions (phase flip, new day).
    // The displayed countdown lives in `SeasonClock`, written 1×/second.
    let mut transition = false;
    let s = season.bypass_change_detection();
    s.remaining -= dt;
    if s.remaining <= 0.0 {
        s.drought = !s.drought;
        s.remaining = if s.drought {
            DROUGHT_LENGTH
        } else {
            WET_LENGTH
        };
        transition = true;
    }
    s.day_timer += dt;
    if s.day_timer >= DAY_LENGTH {
        s.day_timer -= DAY_LENGTH;
        s.day += 1;
        transition = true;
    }
    if transition {
        season.set_changed();
    }
    let seconds = season.remaining.max(0.0).ceil() as u32;
    if clock.0 != seconds {
        clock.0 = seconds;
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

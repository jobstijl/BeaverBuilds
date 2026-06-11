mod bench;
mod bridge;
mod chronicle;
mod interact;
mod intro;
mod render;
mod sim;
mod ui;

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

/// The game starts as a self-playing cinematic intro; any click (or
/// Space/Enter) tears it down and starts a fresh colony for the player.
#[derive(States, Default, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AppState {
    #[default]
    Intro,
    Playing,
}

/// `BB_FAST=1` starts the game at 4x speed (combinable with any mode).
fn apply_speed_env(mut time: ResMut<Time<Virtual>>) {
    if std::env::var("BB_FAST").is_ok() {
        time.set_relative_speed(4.0);
    }
}

fn main() {
    if std::env::var("BB_BENCH").is_ok() {
        bench::run_benchmarks();
        return;
    }
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "BeaverBuilds".into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(MeshPickingPlugin)
        .insert_state(if std::env::var("BB_SKIP_INTRO").is_ok() {
            AppState::Playing
        } else {
            AppState::Intro
        })
        .add_plugins((
            bevy_reactive_bsn::ReactiveBsnPlugin,
            bridge::BridgePlugin,
            chronicle::ChroniclePlugin,
            intro::IntroPlugin,
            sim::SimPlugin,
            render::RenderPlugin,
            interact::InteractPlugin,
            ui::GameUiPlugin,
        ))
        .add_systems(Startup, apply_speed_env)
        .add_systems(Update, debug_screenshot)
        .add_systems(Update, debug_autobuild)
        .run();
}

/// With BB_AUTOBUILD=1, place one of each placeable building shortly after
/// startup. Used for agent self-verification of construction, jobs and the
/// reactive visuals without manual input.
fn debug_autobuild(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut map: ResMut<sim::map::Map>,
    mut stockpile: ResMut<sim::Stockpile>,
    mut selected: ResMut<interact::Selected>,
    mut done: Local<bool>,
) {
    use sim::buildings::{BuildingKind, place_building, placement_error};
    if *done || std::env::var("BB_AUTOBUILD").is_err() || time.elapsed_secs() < 1.5 {
        return;
    }
    *done = true;
    stockpile.logs += 100.0;
    let kinds = [
        BuildingKind::Lodge,
        BuildingKind::WaterPump,
        BuildingKind::CarrotFarm,
        BuildingKind::Lumberjack,
        BuildingKind::Forester,
        BuildingKind::Dam,
        BuildingKind::Path,
        BuildingKind::Path,
        BuildingKind::Path,
    ];
    let center = UVec2::new(map.width / 2, map.height / 2);
    for kind in kinds {
        let spot = (0..map.height)
            .flat_map(|y| (0..map.width).map(move |x| UVec2::new(x, y)))
            .filter(|t| placement_error(&map, kind, *t).is_none())
            .min_by_key(|t| {
                let d = t.as_ivec2() - center.as_ivec2();
                d.x * d.x + d.y * d.y
            });
        if let Some(tile) = spot {
            let entity = place_building(&mut commands, &mut map, &mut stockpile, kind, tile);
            if kind == BuildingKind::Lodge {
                // Exercise the info-panel rebuild reactor.
                selected.0 = Some(entity);
            }
            info!("autobuild: placed {kind:?} at {tile}");
        } else {
            warn!("autobuild: no valid tile for {kind:?}");
        }
    }
}

/// With BB_SHOT=<prefix> set, save numbered screenshots every 10 seconds
/// (and on F12). Used for headless/agent verification.
fn debug_screenshot(
    mut commands: Commands,
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut next: Local<f32>,
    mut count: Local<u32>,
) {
    let Ok(prefix) = std::env::var("BB_SHOT") else {
        return;
    };
    if *next == 0.0 {
        *next = 6.0;
    }
    if time.elapsed_secs() > *next || keys.just_pressed(KeyCode::F12) {
        *next = time.elapsed_secs() + 10.0;
        *count += 1;
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(format!("{prefix}.{:02}.png", *count)));
    }
}

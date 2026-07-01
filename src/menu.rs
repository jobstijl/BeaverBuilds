//! An Esc pause menu, built with Bevy's Feathers widget toolkit. Opening it
//! pauses the colony; it offers Resume, Restart (a fresh colony on a new
//! seed), Quit to title (back to the cinematic attract mode) and Quit to
//! desktop.
//!
//! Esc is layered so it never surprises: if the menu is open it closes; else
//! if a build tool or a selection is active it cancels that (the old Esc
//! behaviour); only with nothing else to back out of does it open the menu.

use bevy::feathers::controls::{ButtonVariant, FeathersButton};
use bevy::feathers::theme::ThemedText;
use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use bevy_reactive_bsn::{Dep, reactive};

use crate::AppState;
use crate::interact::{Selected, Tool};
use crate::sim::{Population, Season};

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuState>()
            .add_systems(Update, handle_escape.run_if(in_state(AppState::Playing)))
            // Leaving Playing for any reason (game over, quit to title, or the
            // identity transition a Restart triggers) tears the menu down.
            .add_systems(OnExit(AppState::Playing), force_close);
    }
}

/// Whether the menu is open, plus the pause state to restore on Resume — the
/// player may have already paused with Space before opening the menu, and
/// closing it shouldn't silently un-pause their game.
#[derive(Resource, Default)]
pub struct MenuState {
    pub open: bool,
    was_paused: bool,
}

/// Run condition for gameplay key handling elsewhere (the speed hotkeys):
/// keys must not pierce the menu — Space/1/2/3 would unpause the colony
/// behind a screen that says PAUSED. `Option` so headless sim tests that
/// don't add `MenuPlugin` keep their hotkeys.
pub fn menu_closed(menu: Option<Res<MenuState>>) -> bool {
    menu.is_none_or(|m| !m.open)
}

/// Root of the spawned menu scene; despawned when the menu closes.
#[derive(Component)]
struct MenuRoot;

fn handle_escape(
    keys: Res<ButtonInput<KeyCode>>,
    mut commands: Commands,
    roots: Query<Entity, With<MenuRoot>>,
    mut menu: ResMut<MenuState>,
    mut tool: ResMut<Tool>,
    mut selected: ResMut<Selected>,
    mut time: ResMut<Time<Virtual>>,
) {
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    if menu.open {
        let resume = !menu.was_paused;
        dismiss_menu(&mut commands, &roots, &mut menu);
        if resume {
            time.unpause();
        }
    } else if *tool != Tool::Select || selected.0.is_some() {
        *tool = Tool::Select;
        selected.0 = None;
    } else {
        open_menu(&mut commands, &mut menu, &mut time);
    }
}

/// Spawn the menu scene and pause the colony, remembering whether the clock
/// was already paused so Resume can restore it.
fn open_menu(commands: &mut Commands, menu: &mut MenuState, time: &mut Time<Virtual>) {
    menu.was_paused = time.is_paused();
    menu.open = true;
    time.pause();
    commands.spawn_scene(menu_scene()).insert(MenuRoot);
}

/// Despawn the menu scene and mark it closed. Does not touch the clock — each
/// caller decides what the game's run state should become.
fn dismiss_menu(
    commands: &mut Commands,
    roots: &Query<Entity, With<MenuRoot>>,
    menu: &mut MenuState,
) {
    for root in roots {
        commands.entity(root).despawn();
    }
    menu.open = false;
}

fn force_close(
    mut commands: Commands,
    roots: Query<Entity, With<MenuRoot>>,
    mut menu: ResMut<MenuState>,
) {
    dismiss_menu(&mut commands, &roots, &mut menu);
}

fn menu_scene() -> impl Scene {
    bsn! {
        // Full-screen dim overlay. Being a normal (pickable) UI node, it sits
        // above the world and swallows clicks, so building placement behind
        // the menu can't happen.
        Node {
            position_type: PositionType::Absolute,
            top: px(0),
            left: px(0),
            right: px(0),
            bottom: px(0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: px(18),
        }
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6))
        Children [
            (
                Text("PAUSED")
                TextFont { font_size: px(44) }
                TextColor(Color::srgb(1.0, 0.92, 0.75))
                TextShadow
            ),
            // A live readout of the colony's state. The clock is paused while
            // the menu is open, so this won't tick — but a reactive fragment
            // is still the right tool: it reads the current values at spawn
            // (and would track them if anything changed the menu open).
            reactive(
                [Dep::resource::<Season>(), Dep::resource::<Population>()],
                |world: &World, _: Entity| {
                    let day = world.resource::<Season>().day;
                    let beavers = world.resource::<Population>().count;
                    let text = format!("Day {day} · {beavers} beavers");
                    bsn! {
                        Text({ text })
                        TextFont { font_size: px(16) }
                        TextColor(Color::srgba(1.0, 1.0, 1.0, 0.75))
                    }
                },
            ),
            (
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Stretch,
                    row_gap: px(8),
                    width: px(220),
                }
                Children [
                    (
                        @FeathersButton { @variant: {ButtonVariant::Primary} }
                        on(|_: On<Activate>,
                           mut commands: Commands,
                           roots: Query<Entity, With<MenuRoot>>,
                           mut menu: ResMut<MenuState>,
                           mut time: ResMut<Time<Virtual>>| {
                            info!(target: "player", "menu: resume");
                            let resume = !menu.was_paused;
                            dismiss_menu(&mut commands, &roots, &mut menu);
                            if resume {
                                time.unpause();
                            }
                        })
                        Children [ (Text("Resume") ThemedText) ]
                    ),
                    (
                        @FeathersButton
                        on(|_: On<Activate>,
                           mut commands: Commands,
                           roots: Query<Entity, With<MenuRoot>>,
                           mut menu: ResMut<MenuState>,
                           mut next: ResMut<NextState<AppState>>| {
                            info!(target: "player", "menu: restart");
                            dismiss_menu(&mut commands, &roots, &mut menu);
                            // Re-entering Playing from Playing is an identity
                            // transition, which `set` still runs — so the
                            // OnEnter(Playing) reset chain founds a fresh colony.
                            next.set(AppState::Playing);
                        })
                        Children [ (Text("Restart") ThemedText) ]
                    ),
                    (
                        @FeathersButton
                        on(|_: On<Activate>,
                           mut commands: Commands,
                           roots: Query<Entity, With<MenuRoot>>,
                           mut menu: ResMut<MenuState>,
                           mut time: ResMut<Time<Virtual>>,
                           mut next: ResMut<NextState<AppState>>| {
                            info!(target: "player", "menu: quit to title");
                            dismiss_menu(&mut commands, &roots, &mut menu);
                            time.unpause();
                            next.set(AppState::Intro);
                        })
                        Children [ (Text("Quit to title") ThemedText) ]
                    ),
                    (
                        @FeathersButton
                        on(|_: On<Activate>, mut exit: MessageWriter<AppExit>| {
                            info!(target: "player", "menu: quit to desktop");
                            exit.write(AppExit::Success);
                        })
                        Children [ (Text("Quit to desktop") ThemedText) ]
                    ),
                ]
            ),
        ]
    }
}

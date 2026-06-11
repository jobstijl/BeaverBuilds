use bevy::prelude::*;

use bevy_reactive_bsn::{
    AsyncValue, Dep, keyed, reactive, reactive_async, reactive_list, reactive_rebuild,
};

use crate::AppState;
use crate::chronicle::Chronicle;
use crate::interact::{Selected, Tool};
use crate::sim::beavers::StarvingCount;
use crate::sim::buildings::{self, BUILDING_DEFS, Building, BuildingDef, UnderConstruction};
use crate::sim::map::Map;
use crate::sim::water::forecast_drought_retention;
use crate::sim::{ForecastTick, Population, Season, Stockpile};

/// All UI is declarative BSN: every dynamic part is a `reactive(..)` fragment
/// embedded directly in the scene tree.
pub struct GameUiPlugin;

impl Plugin for GameUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_ui)
            .add_systems(OnEnter(AppState::Playing), show_hud);
    }
}

/// Marks HUD roots so the intro can keep them hidden until the game starts.
#[derive(Component)]
struct HudRoot;

fn setup_ui(mut commands: Commands, state: Res<State<AppState>>) {
    let hidden = *state.get() == AppState::Intro;
    fn root(commands: &mut Commands, hidden: bool, scene: impl bevy::scene::Scene) {
        let mut entity = commands.spawn_scene(scene);
        entity.insert(HudRoot);
        if hidden {
            entity.insert(Visibility::Hidden);
        }
    }
    root(&mut commands, hidden, top_bar());
    root(&mut commands, hidden, warnings());
    root(&mut commands, hidden, build_menu());
    root(&mut commands, hidden, info_panel());
    root(&mut commands, hidden, hints());
    root(&mut commands, hidden, chronicle_panel());
}

fn show_hud(mut roots: Query<&mut Visibility, With<HudRoot>>) {
    for mut visibility in &mut roots {
        *visibility = Visibility::Visible;
    }
}

const PANEL_BG: Color = Color::srgba(0.08, 0.1, 0.08, 0.88);

fn top_bar() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(0),
            left: px(0),
            right: px(0),
            padding: UiRect::axes(px(14), px(8)),
            column_gap: px(26),
            align_items: AlignItems::Center,
        }
        BackgroundColor({ PANEL_BG })
        Children [
            // Stockpile readout.
            reactive([Dep::resource::<Stockpile>()], |world: &World, _: Entity| {
                let s = world.resource::<Stockpile>();
                let text = format!(
                    "Logs {:.0}    Food {:.0}    Water {:.0}",
                    s.logs.floor(),
                    s.food.floor(),
                    s.water.floor()
                );
                bsn! { Text({ text }) }
            }),
            // Population readout.
            reactive([Dep::resource::<Population>()], |world: &World, _: Entity| {
                    let p = world.resource::<Population>();
                    let text = format!("Beavers {}/{}", p.count, p.cap);
                    bsn! { Text({ text }) }
                }),
            // Calendar / season readout. The projection dep wakes it once
            // per displayed second (and on day/phase flips), not 60×/sec,
            // even though Season's countdown ticks every frame.
            reactive(
                [Dep::resource_value(|s: &Season| {
                    (s.day, s.drought, s.remaining.max(0.0).ceil() as u32)
                })],
                |world: &World, _: Entity| {
                    let s = world.resource::<Season>();
                    let seconds = s.remaining.max(0.0).ceil() as u32;
                    let text = if s.drought {
                        format!("Day {}  ·  DROUGHT  {seconds}s left", s.day)
                    } else {
                        format!("Day {}  ·  Wet season  {seconds}s", s.day)
                    };
                    let color = if s.drought {
                        Color::srgb(1.0, 0.55, 0.25)
                    } else {
                        Color::srgb(0.7, 0.9, 1.0)
                    };
                    bsn! {
                        Text({ text })
                        TextColor({ color })
                    }
                },
            ),
            // Drought forecast: an async resource. Every few seconds (or
            // when a building, e.g. a dam, appears) the water simulation is
            // run through a whole drought on the task pool; the result
            // renders as a colored readout. The host carries a Node so UI
            // layout flows through to the rendered child.
            (
                Node
                reactive_async(
                    [Dep::resource::<ForecastTick>(), Dep::components::<Building>()],
                    |world: &World, _| {
                        let map = world.resource::<Map>();
                        let (ground, dam) = (map.ground.clone(), map.dam.clone());
                        let (drain, water) = (map.drain.clone(), map.water.clone());
                        let (width, height) = (map.width, map.height);
                        async move {
                            forecast_drought_retention(ground, dam, drain, water, width, height)
                        }
                    },
                    |_: &World, _: Entity, value: &AsyncValue<f32>| {
                        let (text, color) = match value.ready() {
                            None => ("Forecast …".to_string(), Color::srgba(1.0, 1.0, 1.0, 0.4)),
                            Some(&fraction) => {
                                let pct = (fraction * 100.0).round();
                                let color = if fraction > 0.5 {
                                    Color::srgb(0.5, 0.9, 0.5)
                                } else if fraction > 0.2 {
                                    Color::srgb(0.95, 0.85, 0.4)
                                } else {
                                    Color::srgb(1.0, 0.45, 0.35)
                                };
                                (format!("Drought forecast: {pct:.0}% water survives"), color)
                            }
                        };
                        bsn! {
                            Text({ text })
                            TextFont { font_size: px(13) }
                            TextColor({ color })
                        }
                    },
                )
            ),
        ]
    }
}

/// Colony warnings: a keyed reactive list — entries appear and disappear as
/// the colony's situation changes. The list manages membership only; each
/// item's content is fixed at spawn.
fn warnings() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(46),
            left: px(0),
            right: px(0),
            justify_content: JustifyContent::Center,
            column_gap: px(8),
        }
        reactive_list(
                [
                    Dep::resource_value(|s: &Season| s.drought),
                    Dep::resource::<Stockpile>(),
                    Dep::resource::<Population>(),
                    Dep::resource::<StarvingCount>(),
                ],
                |world: &World| {
                    let season = world.resource::<Season>();
                    let stock = world.resource::<Stockpile>();
                    let pop = world.resource::<Population>();
                    let starving = world.resource::<StarvingCount>().0;
                    let mut items = Vec::new();
                    let mut warn = |key: u64, text: &str, color: Color| {
                        items.push(keyed(key, warning_badge(text.to_string(), color)));
                    };
                    if season.drought {
                        warn(1, "Drought — the river has stopped, water is evaporating", Color::srgb(1.0, 0.6, 0.2));
                    }
                    if stock.food < 1.0 && pop.count > 0 {
                        warn(2, "No food!", Color::srgb(1.0, 0.35, 0.3));
                    }
                    if stock.water < 1.0 && pop.count > 0 {
                        warn(3, "No drinking water!", Color::srgb(0.5, 0.75, 1.0));
                    }
                    if pop.count >= pop.cap {
                        warn(4, "Housing is full — build a lodge", Color::srgb(0.95, 0.9, 0.5));
                    }
                    if starving > 0 {
                        warn(5, "Beavers are starving!", Color::srgb(1.0, 0.25, 0.2));
                    }
                items
            },
        )
    }
}

fn warning_badge(text: String, color: Color) -> impl bevy::scene::Scene {
    bsn! {
        Node {
            padding: UiRect::axes(px(10), px(4)),
            border_radius: BorderRadius::all(px(5)),
        }
        BackgroundColor(Color::srgba(0.15, 0.05, 0.02, 0.85))
        Children [(
            Text({ text })
            TextFont { font_size: px(13) }
            TextColor({ color })
        )]
    }
}

fn build_menu() -> impl bevy::scene::Scene {
    let buttons: Vec<_> = BUILDING_DEFS.iter().map(build_button).collect();
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            bottom: px(0),
            left: px(0),
            right: px(0),
            padding: UiRect::all(px(10)),
            column_gap: px(8),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::FlexEnd,
        }
        BackgroundColor({ PANEL_BG })
        Children [
            { buttons },
            demolish_button(),
        ]
    }
}

fn build_button(def: &'static BuildingDef) -> impl bevy::scene::Scene {
    let kind = def.kind;
    let cost = def.cost_logs;
    bsn! {
        Button
        Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            padding: UiRect::axes(px(12), px(7)),
            row_gap: px(2),
            border_radius: BorderRadius::all(px(6)),
        }
        on(move |_: On<Pointer<Click>>, mut tool: ResMut<Tool>| {
            *tool = Tool::Build(kind);
        })
        // Background reacts to affordability and tool selection.
        reactive(
                [Dep::resource::<Stockpile>(), Dep::resource::<Tool>()],
                move |world: &World, _: Entity| {
                    let affordable = world.resource::<Stockpile>().logs >= cost;
                    let active = *world.resource::<Tool>() == Tool::Build(kind);
                    let bg = if active {
                        Color::srgb(0.25, 0.45, 0.28)
                    } else if affordable {
                        Color::srgb(0.17, 0.21, 0.17)
                    } else {
                        Color::srgb(0.11, 0.11, 0.11)
                    };
                bsn! { BackgroundColor({ bg }) }
            },
        )
        Children [
            (
                Text({ def.name })
                TextFont { font_size: px(15) }
            ),
            (
                Text({ format!("{cost:.0} logs") })
                TextFont { font_size: px(11) }
                TextColor(Color::srgb(0.75, 0.75, 0.6))
            ),
        ]
    }
}

fn demolish_button() -> impl bevy::scene::Scene {
    bsn! {
        Button
        Node {
            align_items: AlignItems::Center,
            padding: UiRect::axes(px(12), px(14)),
            border_radius: BorderRadius::all(px(6)),
        }
        on(move |_: On<Pointer<Click>>, mut tool: ResMut<Tool>| {
            *tool = Tool::Demolish;
        })
        reactive([Dep::resource::<Tool>()], |world: &World, _: Entity| {
                let active = *world.resource::<Tool>() == Tool::Demolish;
                let bg = if active {
                    Color::srgb(0.5, 0.2, 0.15)
                } else {
                    Color::srgb(0.17, 0.21, 0.17)
                };
            bsn! { BackgroundColor({ bg }) }
        })
        Children [(
            Text("Demolish")
            TextFont { font_size: px(15) }
            TextColor(Color::srgb(1.0, 0.6, 0.5))
        )]
    }
}

/// Inspection panel for the selected building. Structure depends on the
/// selection, so this is a rebuild fragment.
fn info_panel() -> impl bevy::scene::Scene {
    bsn! {
        reactive_rebuild(
                [
                    Dep::resource::<Selected>(),
                    Dep::components::<Building>(),
                    Dep::components::<UnderConstruction>(),
                ],
                |world: &World, _: Entity| {
                    let selected = world.resource::<Selected>().0;
                    let info = selected.and_then(|e| {
                        world.get::<Building>(e).map(|b| {
                            let progress = world
                                .get::<UnderConstruction>(e)
                                .map(|uc| (uc.done / uc.required * 100.0).clamp(0.0, 99.0) as i32);
                            (b.kind, progress)
                        })
                    });
                    let Some((kind, progress)) = info else {
                        return Box::new(bsn! { Node { display: Display::None } })
                            as Box<dyn bevy::scene::Scene>;
                    };
                    let def = buildings::def(kind);
                    let name = def.name;
                    let description = def.description;
                    let status = match progress {
                        Some(p) => format!("Under construction — {p}%"),
                        None => "Operational".to_string(),
                    };
                    let status_color = if progress.is_some() {
                        Color::srgb(0.95, 0.85, 0.4)
                    } else {
                        Color::srgb(0.5, 0.9, 0.5)
                    };
                    Box::new(bsn! {
                        Node {
                            display: Display::Flex,
                            position_type: PositionType::Absolute,
                            right: px(10),
                            top: px(56),
                            width: px(250),
                            flex_direction: FlexDirection::Column,
                            padding: UiRect::all(px(12)),
                            row_gap: px(6),
                            border_radius: BorderRadius::all(px(8)),
                        }
                        BackgroundColor({ PANEL_BG })
                        Children [
                            (
                                Text({ name })
                                TextFont { font_size: px(17) }
                            ),
                            (
                                Text({ status })
                                TextFont { font_size: px(13) }
                                TextColor({ status_color })
                            ),
                            (
                                Text({ description })
                                TextFont { font_size: px(12) }
                                TextColor(Color::srgb(0.75, 0.78, 0.72))
                            ),
                        ]
                }) as Box<dyn bevy::scene::Scene>
            },
        )
    }
}

/// The scribe's daily entries (written by an async task through the
/// world bridge), rendered reactively like any other resource.
fn chronicle_panel() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            right: px(10),
            bottom: px(86),
        }
        reactive([Dep::resource::<Chronicle>()], |world: &World, _: Entity| {
            let chronicle = &world.resource::<Chronicle>().0;
            let start = chronicle.len().saturating_sub(3);
            let text = chronicle[start..].join("\n");
            bsn! {
                Text({ text })
                TextFont { font_size: px(11) }
                TextColor(Color::srgba(1.0, 1.0, 1.0, 0.55))
            }
        })
    }
}

fn hints() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            bottom: px(86),
            left: px(10),
        }
        Children [(
            Text("WASD pan · Q/E rotate · R/F or middle-drag tilt · scroll zoom · click build · right-click cancel · Space pause · 1/2/3 speed")
            TextFont { font_size: px(11) }
            TextColor(Color::srgba(1.0, 1.0, 1.0, 0.45))
        )]
    }
}

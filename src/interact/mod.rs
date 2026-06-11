use bevy::picking::pointer::PointerButton;

use crate::AppState;
use bevy::prelude::*;

use crate::render::{GameAssets, Tile, building_size};
use crate::sim::Stockpile;
use crate::sim::buildings::{self, Building, BuildingKind};
use crate::sim::map::Map;
use bevy_reactive_bsn::{Dep, reactive};

/// The player's active tool.
#[derive(Resource, Default, Clone, Copy, PartialEq)]
pub enum Tool {
    #[default]
    Select,
    Build(BuildingKind),
    Demolish,
}

/// Tile currently under the cursor, if any.
#[derive(Resource, Default)]
pub struct Hover(pub Option<UVec2>);

/// Building currently selected for inspection.
#[derive(Resource, Default)]
pub struct Selected(pub Option<Entity>);

pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Tool>()
            .init_resource::<Hover>()
            .init_resource::<Selected>()
            .add_systems(Startup, spawn_ghost)
            .add_systems(Update, cancel_tool.run_if(in_state(AppState::Playing)))
            .add_observer(track_hover)
            .add_observer(handle_click);
    }
}

fn cancel_tool(
    keys: Res<ButtonInput<KeyCode>>,
    mut tool: ResMut<Tool>,
    mut selected: ResMut<Selected>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        *tool = Tool::Select;
        selected.0 = None;
    }
}

fn track_hover(
    moved: On<Pointer<Move>>,
    state: Res<State<AppState>>,
    tiles: Query<&Tile>,
    mut hover: ResMut<Hover>,
) {
    if *state.get() != AppState::Playing {
        return;
    }
    if let Ok(tile) = tiles.get(moved.entity)
        && hover.0 != Some(tile.0)
    {
        hover.0 = Some(tile.0);
    }
}

/// Walk up the hierarchy from a picked mesh to the building root, if any.
/// Building visuals are multi-part child entities, so clicks land on parts.
fn building_root(
    mut entity: Entity,
    buildings: &Query<&Building>,
    parents: &Query<&ChildOf>,
) -> Option<Entity> {
    loop {
        if buildings.contains(entity) {
            return Some(entity);
        }
        entity = parents.get(entity).ok()?.parent();
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_click(
    mut click: On<Pointer<Click>>,
    state: Res<State<AppState>>,
    mut commands: Commands,
    tiles: Query<&Tile>,
    buildings_q: Query<&Building>,
    parents: Query<&ChildOf>,
    mut map: ResMut<Map>,
    mut stockpile: ResMut<Stockpile>,
    mut tool: ResMut<Tool>,
    mut selected: ResMut<Selected>,
) {
    if *state.get() != AppState::Playing {
        return;
    }
    // Clicks on multi-part buildings bubble from the part to the root; we
    // resolve the root ourselves, so handle each click exactly once (a
    // double-fired demolish refunded twice).
    click.propagate(false);
    if click.button == PointerButton::Secondary {
        *tool = Tool::Select;
        selected.0 = None;
        return;
    }
    if click.button != PointerButton::Primary {
        return;
    }
    let target = click.entity;

    if let Some(root) = building_root(target, &buildings_q, &parents) {
        let building = buildings_q.get(root).unwrap();
        match *tool {
            Tool::Demolish => {
                info!(target: "player", "demolished {:?} at {}", building.kind, building.tile);
                buildings::demolish(&mut commands, &mut map, &mut stockpile, root, building);
                if selected.0 == Some(root) {
                    selected.0 = None;
                }
            }
            _ => selected.0 = Some(root),
        }
        return;
    }

    if let Ok(tile) = tiles.get(target) {
        match *tool {
            Tool::Build(kind) => {
                let affordable = stockpile.logs >= buildings::def(kind).cost_logs;
                if affordable && buildings::placement_error(&map, kind, tile.0).is_none() {
                    info!(target: "player", "placed {kind:?} at {}", tile.0);
                    buildings::place_building(
                        &mut commands,
                        &mut map,
                        &mut stockpile,
                        kind,
                        tile.0,
                    );
                }
            }
            _ => selected.0 = None,
        }
    }
}

/// The placement ghost: a translucent preview at the hovered tile whose
/// visibility, position and color are a reactive function of tool, hover,
/// stockpile and map state.
fn spawn_ghost(mut commands: Commands, assets: Res<GameAssets>) {
    let cube = assets.cube.clone();
    commands.spawn_scene(bsn! {
        template_value(Pickable::IGNORE)
        template_value(Visibility::Hidden)
        reactive(
            [
                Dep::resource::<Tool>(),
                Dep::resource::<Hover>(),
                Dep::resource::<Stockpile>(),
                Dep::resource::<Map>(),
            ],
            move |world: &World, _: Entity| {
            let assets = world.resource::<GameAssets>();
            let map = world.resource::<Map>();
            let hover = world.resource::<Hover>().0;
            let tool = *world.resource::<Tool>();

            let mut visibility = Visibility::Hidden;
            let mut transform = Transform::IDENTITY;
            let mut material = assets.ghost_ok.clone();
            if let (Tool::Build(kind), Some(tile)) = (tool, hover) {
                visibility = Visibility::Visible;
                let ok = buildings::placement_error(map, kind, tile).is_none()
                    && world.resource::<Stockpile>().logs >= buildings::def(kind).cost_logs;
                material = if ok {
                    assets.ghost_ok.clone()
                } else {
                    assets.ghost_bad.clone()
                };
                let size = building_size(kind);
                let base = map.tile_center(tile.x, tile.y);
                transform =
                    Transform::from_translation(base + Vec3::Y * (size.y / 2.0)).with_scale(size);
            }
                let mesh = cube.clone();
                bsn! {
                    Mesh3d({ mesh })
                    MeshMaterial3d::<StandardMaterial>({ material })
                    template_value(transform)
                    template_value(visibility)
                }
            },
        )
    });
}

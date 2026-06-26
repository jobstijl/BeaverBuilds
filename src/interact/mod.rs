use bevy::picking::pointer::PointerButton;

use crate::AppState;
use bevy::prelude::*;

use crate::render::{GameAssets, Tile, building_size};
use crate::sim::Stockpile;
use crate::sim::buildings::{self, Building, BuildingKind};
use crate::sim::map::Map;
use bevy_reactive_bsn::{Dep, reactive};

/// The player's active tool.
#[derive(Resource, Default, Clone, Copy, PartialEq, Debug)]
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

/// A short-lived on-screen notice (e.g. why a placement was rejected).
#[derive(Resource, Default)]
pub struct Notice {
    pub message: Option<String>,
    pub expires: f32,
}

pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Tool>()
            .init_resource::<Hover>()
            .init_resource::<Selected>()
            .init_resource::<Notice>()
            .add_systems(Startup, spawn_ghost)
            .add_systems(Update, expire_notice.run_if(in_state(AppState::Playing)))
            .add_observer(track_hover)
            .add_observer(handle_click);
    }
}

fn expire_notice(time: Res<Time<Real>>, mut notice: ResMut<Notice>) {
    if notice.message.is_some() && time.elapsed_secs() > notice.expires {
        notice.bypass_change_detection().message = None;
        notice.set_changed();
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
    mut notice: ResMut<Notice>,
    time: Res<Time<Real>>,
) {
    if *state.get() != AppState::Playing {
        return;
    }
    if click.button == PointerButton::Secondary {
        *tool = Tool::Select;
        selected.0 = None;
        return;
    }
    if click.button != PointerButton::Primary {
        return;
    }
    let target = click.entity;
    debug!(target: "player", "click on {target} with {:?}", *tool);

    if let Some(root) = building_root(target, &buildings_q, &parents) {
        // We resolved the root ourselves, so stop the bubble here: clicks on
        // multi-part buildings would otherwise fire this observer again at
        // the root (a double-fired demolish once refunded twice). UI clicks
        // never reach this branch and keep bubbling to their buttons.
        click.propagate(false);
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
        click.propagate(false);
        match *tool {
            Tool::Build(kind) => {
                let rejection = if stockpile.logs < buildings::def(kind).cost_logs {
                    Some("not enough logs")
                } else {
                    buildings::placement_error(&map, kind, tile.0)
                };
                match rejection {
                    None => {
                        info!(target: "player", "placed {kind:?} at {}", tile.0);
                        buildings::place_building(
                            &mut commands,
                            &mut map,
                            &mut stockpile,
                            kind,
                            tile.0,
                        );
                    }
                    Some(reason) => {
                        info!(target: "player", "rejected {kind:?} at {}: {reason}", tile.0);
                        notice.message = Some(format!("Can't build here: {reason}"));
                        notice.expires = time.elapsed_secs() + 2.2;
                    }
                }
            }
            _ => selected.0 = None,
        }
    }
    // Anything else (UI nodes, empty space): leave propagation alone so
    // button observers up the hierarchy still fire.
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

pub mod camera;

use bevy::prelude::*;
use bevy_reactive_bsn::{Dep, Reactor, ReactorSpec, reactive, reactive_rebuild};

use crate::sim::Season;
use crate::sim::beavers::{Beaver, Starving};
use crate::sim::buildings::{Building, BuildingKind, UnderConstruction};
use crate::sim::map::{LEVEL, Map};
use crate::sim::trees::Tree;
use crate::sim::water::WaterSet;

/// Everything visible in the world is expressed as BSN scenes. Static things
/// (terrain blocks) are spawned once with `bsn!`; everything whose look
/// depends on game state carries a reactor so the visual *is* a reactive
/// function of that state: water depth, tree growth, construction progress,
/// starving beavers, drought lighting and fog.
pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(SKY))
            .add_plugins(camera::CameraPlugin)
            .add_systems(PreStartup, load_assets)
            .add_systems(Startup, (spawn_terrain, spawn_lights))
            .add_systems(FixedUpdate, mirror_tile_state.after(WaterSet))
            .add_observer(dress_tree)
            .add_observer(dress_building)
            .add_observer(dress_beaver);
    }
}

pub const SKY: Color = Color::srgb(0.55, 0.71, 0.86);
pub const SKY_DROUGHT: Color = Color::srgb(0.78, 0.68, 0.5);

#[derive(Resource)]
pub struct GameAssets {
    // Unit primitives, shaped per use via `Transform` scale.
    pub cube: Handle<Mesh>,
    pub cylinder: Handle<Mesh>,
    pub sphere: Handle<Mesh>,
    pub capsule: Handle<Mesh>,
    // Palette.
    pub grass: Handle<StandardMaterial>,
    pub grass_alt: Handle<StandardMaterial>,
    pub grass_dry: Handle<StandardMaterial>,
    pub grass_dry_alt: Handle<StandardMaterial>,
    pub river_bed: Handle<StandardMaterial>,
    pub water: Handle<StandardMaterial>,
    pub wood: Handle<StandardMaterial>,
    pub wood_dark: Handle<StandardMaterial>,
    pub plank: Handle<StandardMaterial>,
    pub stone: Handle<StandardMaterial>,
    pub flag_red: Handle<StandardMaterial>,
    pub leaf: [Handle<StandardMaterial>; 3],
    pub soil: Handle<StandardMaterial>,
    pub carrot_top: Handle<StandardMaterial>,
    pub tank_blue: Handle<StandardMaterial>,
    pub beaver_fur: Handle<StandardMaterial>,
    pub beaver_fur_starving: Handle<StandardMaterial>,
    pub beaver_dark: Handle<StandardMaterial>,
    pub construction: Handle<StandardMaterial>,
    pub ghost_ok: Handle<StandardMaterial>,
    pub ghost_bad: Handle<StandardMaterial>,
}

fn solid(materials: &mut Assets<StandardMaterial>, c: Color) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: c,
        perceptual_roughness: 0.92,
        ..default()
    })
}

fn ghost(materials: &mut Assets<StandardMaterial>, c: Color) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: c,
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    })
}

fn load_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let m = &mut *materials;
    commands.insert_resource(GameAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        cylinder: meshes.add(Cylinder::new(0.5, 1.0)),
        sphere: meshes.add(Sphere::new(0.5)),
        capsule: meshes.add(Capsule3d::new(0.5, 0.6)),
        grass: solid(m, Color::srgb(0.4, 0.62, 0.3)),
        grass_alt: solid(m, Color::srgb(0.37, 0.585, 0.285)),
        grass_dry: solid(m, Color::srgb(0.74, 0.64, 0.4)),
        grass_dry_alt: solid(m, Color::srgb(0.705, 0.61, 0.375)),
        river_bed: solid(m, Color::srgb(0.8, 0.72, 0.54)),
        water: m.add(StandardMaterial {
            base_color: Color::srgba(0.2, 0.5, 0.85, 0.62),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.15,
            reflectance: 0.7,
            ..default()
        }),
        wood: solid(m, Color::srgb(0.55, 0.38, 0.22)),
        wood_dark: solid(m, Color::srgb(0.38, 0.25, 0.15)),
        plank: solid(m, Color::srgb(0.67, 0.5, 0.3)),
        stone: solid(m, Color::srgb(0.62, 0.58, 0.5)),
        flag_red: solid(m, Color::srgb(0.85, 0.22, 0.16)),
        leaf: [
            solid(m, Color::srgb(0.2, 0.45, 0.21)),
            solid(m, Color::srgb(0.26, 0.52, 0.24)),
            solid(m, Color::srgb(0.16, 0.4, 0.2)),
        ],
        soil: solid(m, Color::srgb(0.36, 0.25, 0.16)),
        carrot_top: solid(m, Color::srgb(0.32, 0.65, 0.26)),
        tank_blue: solid(m, Color::srgb(0.28, 0.46, 0.68)),
        beaver_fur: solid(m, Color::srgb(0.52, 0.36, 0.22)),
        beaver_fur_starving: solid(m, Color::srgb(0.6, 0.26, 0.2)),
        beaver_dark: solid(m, Color::srgb(0.32, 0.21, 0.13)),
        construction: solid(m, Color::srgb(0.56, 0.55, 0.52)),
        ghost_ok: ghost(m, Color::srgba(0.3, 0.9, 0.4, 0.45)),
        ghost_bad: ghost(m, Color::srgba(0.95, 0.25, 0.2, 0.45)),
    });
}

/// Marks a terrain tile (clickable ground).
#[derive(Component, Clone, Default)]
pub struct Tile(pub UVec2);

/// Per-tile mirrored sim state. The water cellular automaton works on the
/// `Map` arrays for speed; this component is only written when a tile
/// *meaningfully* changes, which is what makes per-tile reactivity
/// fine-grained instead of "the whole map changed every tick".
#[derive(Component, Clone, Default)]
pub struct TileState {
    pub depth: f32,
    pub irrigated: bool,
}

/// Ground/water entity per tile index, for the mirror system.
#[derive(Resource)]
pub struct TileEntities {
    pub ground: Vec<Entity>,
    pub water: Vec<Entity>,
}

/// Footprint used by the placement ghost.
pub fn building_size(kind: BuildingKind) -> Vec3 {
    match kind {
        BuildingKind::Lodge => Vec3::new(0.9, 0.8, 0.9),
        BuildingKind::WaterPump => Vec3::new(0.5, 1.0, 0.5),
        BuildingKind::CarrotFarm => Vec3::new(0.95, 0.18, 0.95),
        BuildingKind::Lumberjack => Vec3::new(0.4, 1.0, 0.4),
        BuildingKind::Forester => Vec3::new(0.6, 0.7, 0.6),
        BuildingKind::Dam => Vec3::new(1.0, 0.7, 0.8),
        BuildingKind::Path => Vec3::new(0.98, 0.08, 0.98),
    }
}

pub fn spawn_terrain(mut commands: Commands, map: Res<Map>, assets: Res<GameAssets>) {
    let n = (map.width * map.height) as usize;
    let mut entities = TileEntities {
        ground: Vec::with_capacity(n),
        water: Vec::with_capacity(n),
    };
    // One shared spec for all grass tiles; each tile forks an instance via
    // the imperative API (`Reactor::from_spec`) instead of inline
    // `reactive(...)`, so thousands of tiles share a single allocation.
    let tint_spec = ReactorSpec::patch(
        [Dep::this::<TileState>()],
        |world: &World, entity: Entity| {
            let assets = world.resource::<GameAssets>();
            let irrigated = world.get::<TileState>(entity).is_some_and(|t| t.irrigated);
            // Checkered two-tone grass; the alternate shade follows the
            // tile's parity so the pattern survives tint flips.
            let alt = world
                .get::<Tile>(entity)
                .is_some_and(|t| (t.0.x + t.0.y) % 2 == 1);
            let mat = match (irrigated, alt) {
                (true, false) => assets.grass.clone(),
                (true, true) => assets.grass_alt.clone(),
                (false, false) => assets.grass_dry.clone(),
                (false, true) => assets.grass_dry_alt.clone(),
            };
            bsn! { MeshMaterial3d::<StandardMaterial>({ mat }) }
        },
    );
    for y in 0..map.height {
        for x in 0..map.width {
            let i = map.idx(x, y);
            let ground = map.ground[i];
            let center = map.tile_center(x, y);
            let height = (ground as f32 * LEVEL).max(0.12);
            let material = if ground == 0 {
                assets.river_bed.clone()
            } else if (x + y) % 2 == 1 {
                assets.grass_alt.clone()
            } else {
                assets.grass.clone()
            };
            let tile_mesh = assets.cube.clone();
            let tile = UVec2::new(x, y);

            // The ground block: a static BSN scene.
            let entity = commands
                .spawn_scene(bsn! {
                    template_value(Tile(tile))
                    Mesh3d({ tile_mesh })
                    MeshMaterial3d::<StandardMaterial>({ material })
                    template_value(
                        Transform::from_translation(Vec3::new(center.x, height / 2.0, center.z))
                            .with_scale(Vec3::new(1.0, height, 1.0))
                    )
                })
                .id();
            entities.ground.push(entity);

            // Reactive grass tint: dries out when irrigation goes away.
            if ground > 0 {
                commands
                    .entity(entity)
                    .insert((TileState::default(), Reactor::from_spec(tint_spec.clone())));
            }

            // The water block above the tile: its whole visual is a reactive
            // function of the mirrored water depth, declared inline.
            let water_mesh = assets.cube.clone();
            let water_mat = assets.water.clone();
            let ground_top = ground as f32 * LEVEL;
            let water_entity = commands
                .spawn_scene(bsn! {
                    TileState
                    template_value(Pickable::IGNORE)
                    reactive(
                        [Dep::this::<TileState>()],
                        move |world: &World, entity: Entity| {
                            let depth = world
                                .get::<TileState>(entity)
                                .map(|t| t.depth)
                                .unwrap_or(0.0);
                            let h = depth * LEVEL;
                            let visible = if depth > 0.02 {
                                Visibility::Visible
                            } else {
                                Visibility::Hidden
                            };
                            let mesh = water_mesh.clone();
                            let mat = water_mat.clone();
                            bsn! {
                                Mesh3d({ mesh })
                                MeshMaterial3d::<StandardMaterial>({ mat })
                                template_value(
                                    Transform::from_translation(Vec3::new(
                                        center.x,
                                        ground_top + h / 2.0,
                                        center.z,
                                    ))
                                    .with_scale(Vec3::new(1.0, h.max(0.001), 1.0))
                                )
                                template_value(visible)
                            }
                        },
                    )
                })
                .id();
            entities.water.push(water_entity);
        }
    }
    commands.insert_resource(entities);
}

/// Copy per-tile water/irrigation out of the `Map` arrays into `TileState`
/// components, writing only on meaningful change so change detection (and
/// therefore the reactors) stays quiet for untouched tiles.
fn mirror_tile_state(
    map: Res<Map>,
    entities: Res<TileEntities>,
    mut states: Query<&mut TileState>,
) {
    for i in 0..map.water.len() {
        if let Ok(mut state) = states.get_mut(entities.water[i])
            && ((state.depth - map.water[i]).abs() > 0.01
                || (state.depth != 0.0 && map.water[i] == 0.0))
        {
            state.depth = map.water[i];
        }
        if let Ok(mut state) = states.get_mut(entities.ground[i])
            && state.irrigated != map.irrigated[i]
        {
            state.irrigated = map.irrigated[i];
        }
    }
}

fn spawn_lights(mut commands: Commands) {
    // Warm key light with shadows; harsher and warmer during droughts. The
    // light itself is a reactive function of the season, declared inline.
    commands.spawn_scene(bsn! {
        template_value(Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.7, 0.0)))
        reactive(
            [Dep::resource_value(|s: &Season| s.drought)],
            |world: &World, _: Entity| {
            let drought = world.resource::<Season>().drought;
            let (color, lux) = if drought {
                (Color::srgb(1.0, 0.82, 0.55), 14_000.0)
            } else {
                (Color::srgb(1.0, 0.97, 0.9), 10_500.0)
            };
            bsn! {
                DirectionalLight {
                    color: { color },
                    illuminance: { lux },
                    shadow_maps_enabled: true,
                }
            }
        },
        )
    });
    // Cool fill from the opposite side so shadowed faces aren't flat black.
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.6, 0.72, 0.9),
            illuminance: 2_300.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -2.3, -0.8, 0.0)),
    ));
}

/// Give a freshly spawned tree its looks: trunk plus a two-sphere canopy in
/// one of three leaf greens (picked per tile), and a reactor that scales the
/// whole tree with its growth.
fn dress_tree(
    add: On<Add, Tree>,
    mut commands: Commands,
    map: Res<Map>,
    trees: Query<&Tree>,
    assets: Res<GameAssets>,
) {
    let entity = add.entity;
    let Ok(tree) = trees.get(entity) else { return };
    let pos = map.tile_center(tree.tile.x, tree.tile.y);
    // Per-tree jitter (deterministic from the tile) so the forest reads
    // organic rather than stamped.
    let hash = tree
        .tile
        .x
        .wrapping_mul(31)
        .wrapping_add(tree.tile.y.wrapping_mul(17));
    let spin = Quat::from_rotation_y((hash % 628) as f32 / 100.0);
    let size = 0.88 + (hash % 25) as f32 / 100.0;
    let trunk = assets.cylinder.clone();
    let trunk_mat = assets.wood_dark.clone();
    let canopy = assets.sphere.clone();
    let canopy2 = assets.sphere.clone();
    let leaf = assets.leaf[((tree.tile.x * 7 + tree.tile.y * 13) % 3) as usize].clone();
    let leaf2 = leaf.clone();
    commands.entity(entity).apply_scene(bsn! {
        template_value(
            Transform::from_translation(pos)
                .with_rotation(spin)
                .with_scale(Vec3::splat(0.05))
        )
        Visibility
        template_value(Pickable::IGNORE)
        reactive([Dep::this::<Tree>()], move |world: &World, entity: Entity| {
            let growth = world.get::<Tree>(entity).map(|t| t.growth).unwrap_or(0.0);
            let scale = (0.25 + 0.75 * growth) * size;
            bsn! {
                template_value(
                    Transform::from_translation(pos)
                        .with_rotation(spin)
                        .with_scale(Vec3::splat(scale))
                )
            }
        })
        Children [
            (
                Mesh3d({ trunk })
                MeshMaterial3d::<StandardMaterial>({ trunk_mat })
                template_value(
                    Transform::from_xyz(0.0, 0.25, 0.0).with_scale(Vec3::new(0.16, 0.5, 0.16))
                )
                template_value(Pickable::IGNORE)
            ),
            (
                Mesh3d({ canopy })
                MeshMaterial3d::<StandardMaterial>({ leaf })
                template_value(
                    Transform::from_xyz(0.0, 0.58, 0.0).with_scale(Vec3::new(0.62, 0.56, 0.62))
                )
                template_value(Pickable::IGNORE)
            ),
            (
                Mesh3d({ canopy2 })
                MeshMaterial3d::<StandardMaterial>({ leaf2 })
                template_value(
                    Transform::from_xyz(0.1, 0.82, 0.04).with_scale(Vec3::splat(0.38))
                )
                template_value(Pickable::IGNORE)
            ),
        ]
    });
}

/// One part of a building: a unit primitive, a material, and a transform.
fn part(
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    transform: Transform,
) -> impl bevy::scene::Scene {
    bsn! {
        Mesh3d({ mesh })
        MeshMaterial3d::<StandardMaterial>({ material })
        template_value(transform)
    }
}

/// The finished look of each building, as a list of parts relative to the
/// tile center, spawned as children of the building root.
fn building_parts(kind: BuildingKind, a: &GameAssets) -> Vec<Box<dyn bevy::scene::Scene>> {
    let at = |x: f32, y: f32, z: f32, sx: f32, sy: f32, sz: f32| {
        Transform::from_xyz(x, y, z).with_scale(Vec3::new(sx, sy, sz))
    };
    // A cube rotated 45° reads as a ridged roof.
    let roof = |y: f32, w: f32, d: f32| {
        Transform::from_xyz(0.0, y, 0.0)
            .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_4))
            .with_scale(Vec3::new(w, w, d))
    };
    let lying = |t: Transform| t.with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
    let p = |mesh: &Handle<Mesh>, mat: &Handle<StandardMaterial>, t: Transform| {
        Box::new(part(mesh.clone(), mat.clone(), t)) as Box<dyn bevy::scene::Scene>
    };
    match kind {
        BuildingKind::Lodge => vec![
            p(&a.cube, &a.wood, at(0.0, 0.21, 0.0, 0.85, 0.42, 0.85)),
            p(&a.cube, &a.wood_dark, roof(0.46, 0.58, 0.95)),
            p(
                &a.cube,
                &a.beaver_dark,
                at(0.0, 0.13, 0.44, 0.2, 0.26, 0.04),
            ),
        ],
        BuildingKind::WaterPump => vec![
            p(
                &a.cylinder,
                &a.tank_blue,
                at(0.0, 0.52, 0.0, 0.42, 0.5, 0.42),
            ),
            p(&a.cube, &a.wood, at(-0.2, 0.16, 0.0, 0.08, 0.32, 0.08)),
            p(&a.cube, &a.wood, at(0.2, 0.16, 0.0, 0.08, 0.32, 0.08)),
            p(&a.cube, &a.wood_dark, at(0.0, 0.84, 0.0, 0.55, 0.06, 0.1)),
        ],
        BuildingKind::CarrotFarm => {
            let mut parts = vec![p(&a.cube, &a.soil, at(0.0, 0.05, 0.0, 0.92, 0.1, 0.92))];
            for (i, &x) in [-0.28f32, 0.0, 0.28].iter().enumerate() {
                for (j, &z) in [-0.22f32, 0.22].iter().enumerate() {
                    let wobble = ((i * 2 + j) % 3) as f32 * 0.02;
                    parts.push(p(
                        &a.sphere,
                        &a.carrot_top,
                        at(x, 0.13 + wobble, z, 0.16, 0.13, 0.16),
                    ));
                }
            }
            parts
        }
        BuildingKind::Lumberjack => vec![
            p(
                &a.cylinder,
                &a.wood_dark,
                at(0.0, 0.5, 0.0, 0.08, 1.0, 0.08),
            ),
            p(&a.cube, &a.flag_red, at(0.17, 0.86, 0.0, 0.3, 0.18, 0.02)),
            p(
                &a.cylinder,
                &a.wood,
                lying(at(-0.14, 0.07, 0.0, 0.14, 0.5, 0.14)),
            ),
            p(
                &a.cylinder,
                &a.wood,
                lying(at(0.14, 0.07, 0.1, 0.14, 0.45, 0.14)),
            ),
        ],
        BuildingKind::Forester => vec![
            p(&a.cylinder, &a.wood_dark, at(0.0, 0.2, 0.0, 0.1, 0.4, 0.1)),
            p(&a.sphere, &a.leaf[1], at(0.0, 0.5, 0.0, 0.46, 0.42, 0.46)),
            p(&a.cube, &a.wood, at(0.3, 0.09, 0.3, 0.28, 0.18, 0.28)),
            p(&a.sphere, &a.leaf[0], at(0.26, 0.22, 0.26, 0.1, 0.1, 0.1)),
            p(&a.sphere, &a.leaf[2], at(0.34, 0.22, 0.34, 0.1, 0.1, 0.1)),
        ],
        BuildingKind::Dam => vec![
            p(&a.cube, &a.wood_dark, at(0.0, 0.26, 0.0, 1.0, 0.52, 0.7)),
            p(&a.cube, &a.plank, at(-0.3, 0.3, 0.0, 0.14, 0.62, 0.74)),
            p(&a.cube, &a.plank, at(0.0, 0.3, 0.0, 0.14, 0.62, 0.74)),
            p(&a.cube, &a.plank, at(0.3, 0.3, 0.0, 0.14, 0.62, 0.74)),
            p(&a.cube, &a.stone, at(0.0, 0.6, 0.0, 1.0, 0.07, 0.34)),
        ],
        BuildingKind::Path => vec![
            p(&a.cube, &a.stone, at(0.0, 0.035, 0.0, 0.98, 0.07, 0.98)),
            p(
                &a.cube,
                &a.river_bed,
                at(0.22, 0.072, 0.18, 0.2, 0.012, 0.26),
            ),
            p(
                &a.cube,
                &a.river_bed,
                at(-0.25, 0.072, -0.2, 0.24, 0.012, 0.2),
            ),
        ],
    }
}

/// Building visual: the root is an invisible anchor at the tile; a rebuild
/// reactor keyed on the *presence* of `UnderConstruction` swaps between a
/// construction site (whose growing volume is itself a nested reactor
/// watching the progress value) and the finished multi-part look.
fn dress_building(
    add: On<Add, Building>,
    mut commands: Commands,
    map: Res<Map>,
    buildings: Query<&Building>,
    assets: Res<GameAssets>,
) {
    let entity = add.entity;
    let Ok(building) = buildings.get(entity) else {
        return;
    };
    let kind = building.kind;
    let base = map.tile_center(building.tile.x, building.tile.y);
    let cube = assets.cube.clone();
    let gray = assets.construction.clone();
    let log_mesh = assets.cylinder.clone();
    let log_mat = assets.wood.clone();
    let size = building_size(kind);
    commands.entity(entity).apply_scene(bsn! {
        template_value(Transform::from_translation(base))
        Visibility
        reactive_rebuild(
            [Dep::presence_this::<UnderConstruction>()],
            move |world: &World, root: Entity| {
                if world.get::<UnderConstruction>(root).is_none() {
                    let assets = world.resource::<GameAssets>();
                    let parts = building_parts(kind, assets);
                    return Box::new(bsn! { Children [ { parts } ] })
                        as Box<dyn bevy::scene::Scene>;
                }
                // Construction site: a scattered log plus a gray volume that
                // grows with build progress (a nested per-value reactor).
                let cube = cube.clone();
                let gray = gray.clone();
                let log_mesh = log_mesh.clone();
                let log_mat = log_mat.clone();
                Box::new(bsn! {
                    Children [
                        (
                            Mesh3d({ cube })
                            MeshMaterial3d::<StandardMaterial>({ gray })
                            reactive(
                                [Dep::entity::<UnderConstruction>(root)],
                                move |world: &World, _: Entity| {
                                    let progress = world
                                        .get::<UnderConstruction>(root)
                                        .map(|uc| (uc.done / uc.required).clamp(0.0, 1.0))
                                        .unwrap_or(1.0);
                                    let h = (size.y * progress).max(0.05);
                                    bsn! {
                                        template_value(
                                            Transform::from_xyz(0.0, h / 2.0, 0.0).with_scale(
                                                Vec3::new(size.x * 0.8, h, size.z * 0.8)
                                            )
                                        )
                                    }
                                },
                            )
                        ),
                        (
                            Mesh3d({ log_mesh })
                            MeshMaterial3d::<StandardMaterial>({ log_mat })
                            template_value(
                                Transform::from_xyz(0.38, 0.06, 0.3)
                                    .with_rotation(Quat::from_rotation_x(
                                        std::f32::consts::FRAC_PI_2
                                    ))
                                    .with_scale(Vec3::new(0.12, 0.45, 0.12))
                            )
                        ),
                    ]
                }) as Box<dyn bevy::scene::Scene>
            },
        )
    });
}

/// Beaver visual: body, head and flat tail. Rebuilt on `Starving`
/// transitions so the fur tint can swap; the root transform stays untouched
/// (the movement system owns it).
fn dress_beaver(add: On<Add, Beaver>, mut commands: Commands, assets: Res<GameAssets>) {
    let entity = add.entity;
    let capsule = assets.capsule.clone();
    let sphere = assets.sphere.clone();
    let cube = assets.cube.clone();
    commands.entity(entity).apply_scene(bsn! {
        Visibility
        template_value(Pickable::IGNORE)
        reactive_rebuild(
            [Dep::this::<Starving>()],
            move |world: &World, root: Entity| {
                let assets = world.resource::<GameAssets>();
                let fur = if world.get::<Starving>(root).is_some() {
                    assets.beaver_fur_starving.clone()
                } else {
                    assets.beaver_fur.clone()
                };
                let fur_head = fur.clone();
                let dark = assets.beaver_dark.clone();
                let capsule = capsule.clone();
                let sphere = sphere.clone();
                let cube = cube.clone();
                bsn! {
                    Children [
                        (
                            Mesh3d({ capsule })
                            MeshMaterial3d::<StandardMaterial>({ fur })
                            template_value(
                                Transform::from_xyz(0.0, 0.15, 0.0)
                                    .with_rotation(Quat::from_rotation_x(
                                        std::f32::consts::FRAC_PI_2
                                    ))
                                    .with_scale(Vec3::new(0.26, 0.3, 0.24))
                            )
                            template_value(Pickable::IGNORE)
                        ),
                        (
                            Mesh3d({ sphere })
                            MeshMaterial3d::<StandardMaterial>({ fur_head })
                            template_value(
                                Transform::from_xyz(0.0, 0.22, 0.2).with_scale(Vec3::splat(0.18))
                            )
                            template_value(Pickable::IGNORE)
                        ),
                        (
                            Mesh3d({ cube })
                            MeshMaterial3d::<StandardMaterial>({ dark })
                            template_value(
                                Transform::from_xyz(0.0, 0.07, -0.26)
                                    .with_scale(Vec3::new(0.14, 0.04, 0.2))
                            )
                            template_value(Pickable::IGNORE)
                        ),
                    ]
                }
            },
        )
    });
}

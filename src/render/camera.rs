use bevy::input::mouse::MouseWheel;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy_reactive_bsn::{Dep, Reactor};

use crate::sim::Season;

/// Simple orbit/pan camera: WASD pans, Q/E rotates, scroll zooms.
/// Carries the ambient light and the (drought-reactive) distance fog.
pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraRig>()
            .add_systems(Startup, spawn_camera)
            .add_systems(Update, (drive_camera, apply_camera).chain());
    }
}

#[derive(Resource)]
pub struct CameraRig {
    pub focus: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            focus: Vec3::ZERO,
            yaw: 0.6,
            pitch: 0.9,
            distance: 24.0,
        }
    }
}

fn spawn_camera(mut commands: Commands) {
    let camera = commands
        .spawn((
            Camera3d::default(),
            Transform::default(),
            // AmbientLight is a per-camera component as of 0.19.
            AmbientLight {
                color: Color::srgb(0.85, 0.9, 1.0),
                brightness: 320.0,
                ..default()
            },
            DistanceFog {
                color: super::SKY,
                falloff: FogFalloff::Linear {
                    start: 38.0,
                    end: 130.0,
                },
                ..default()
            },
        ))
        .id();
    // Atmosphere as a reactive function of the season: drought turns the
    // haze dusty and pulls it closer. Attached imperatively (the camera is
    // spawned as a plain bundle) — the attach-to-existing-entity API.
    commands.entity(camera).insert(Reactor::patch(
        [Dep::resource_value(|s: &Season| s.drought)],
        |world: &World, _: Entity| {
            let drought = world.resource::<Season>().drought;
            let (color, start, end) = if drought {
                (super::SKY_DROUGHT, 28.0, 100.0)
            } else {
                (super::SKY, 38.0, 130.0)
            };
            bsn! {
                template_value(DistanceFog {
                    color,
                    falloff: FogFalloff::Linear { start, end },
                    ..Default::default()
                })
            }
        },
    ));
}

fn drive_camera(
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<MouseWheel>,
    time: Res<Time<Real>>,
    mut rig: ResMut<CameraRig>,
) {
    let dt = time.delta_secs();
    let mut pan = Vec2::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        pan.y -= 1.0;
    }
    if keys.pressed(KeyCode::KeyS) {
        pan.y += 1.0;
    }
    if keys.pressed(KeyCode::KeyA) {
        pan.x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) {
        pan.x += 1.0;
    }
    if pan != Vec2::ZERO {
        let speed = rig.distance * 0.6;
        let forward = Vec2::from_angle(-rig.yaw).rotate(pan) * speed * dt;
        rig.focus += Vec3::new(forward.x, 0.0, forward.y);
        rig.focus = rig.focus.clamp(Vec3::splat(-30.0), Vec3::splat(30.0));
    }
    if keys.pressed(KeyCode::KeyQ) {
        rig.yaw += 1.5 * dt;
    }
    if keys.pressed(KeyCode::KeyE) {
        rig.yaw -= 1.5 * dt;
    }
    for ev in wheel.read() {
        rig.distance = (rig.distance - ev.y * 2.0).clamp(6.0, 60.0);
    }
}

fn apply_camera(rig: Res<CameraRig>, mut camera: Query<&mut Transform, With<Camera3d>>) {
    let Ok(mut transform) = camera.single_mut() else {
        return;
    };
    let rot = Quat::from_euler(EulerRot::YXZ, rig.yaw, -rig.pitch, 0.0);
    transform.translation = rig.focus + rot * (Vec3::Z * rig.distance);
    transform.look_at(rig.focus, Vec3::Y);
}

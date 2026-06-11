//! The classic reactive counter: a count readout, +/- buttons, and a keyed
//! list of squares that grows and shrinks with the count.
//!
//! Run with: `cargo run -p bevy_reactive_bsn --example counter`

use bevy::prelude::*;
use bevy_reactive_bsn::{Dep, ReactiveBsnPlugin, keyed, reactive, reactive_list};

#[derive(Resource, Default)]
struct Counter(i32);

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(ReactiveBsnPlugin)
        .init_resource::<Counter>()
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2d);
    commands.spawn_scene(ui());
}

fn ui() -> impl Scene {
    bsn! {
        Node {
            width: percent(100),
            height: percent(100),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: px(16),
        }
        Children [
            (
                Node { column_gap: px(12), align_items: AlignItems::Center }
                Children [
                    button("-", -1),
                    // The readout: re-renders only when Counter changes.
                    reactive([Dep::resource::<Counter>()], |world: &World, _: Entity| {
                        let count = world.resource::<Counter>().0;
                        bsn! {
                            Text({ format!("{count}") })
                            TextFont { font_size: px(40) }
                        }
                    }),
                    button("+", 1),
                ]
            ),
            // One square per count: keyed membership reconciliation —
            // existing squares are never touched when the count changes.
            (
                Node { column_gap: px(6) }
                reactive_list([Dep::resource::<Counter>()], |world: &World| {
                    let count = world.resource::<Counter>().0.max(0) as u64;
                    (0..count)
                        .map(|key| {
                            let hue = (key as f32 * 47.0) % 360.0;
                            keyed(key, bsn! {
                                Node { width: px(24), height: px(24) }
                                BackgroundColor({ Color::hsl(hue, 0.8, 0.6) })
                            })
                        })
                        .collect()
                })
            ),
        ]
    }
}

fn button(label: &'static str, delta: i32) -> impl Scene {
    bsn! {
        Button
        Node {
            width: px(56),
            height: px(56),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border_radius: BorderRadius::all(px(8)),
        }
        BackgroundColor(Color::srgb(0.2, 0.25, 0.3))
        on(move |_: On<Pointer<Click>>, mut counter: ResMut<Counter>| {
            counter.0 += delta;
        })
        Children [(
            Text({ label })
            TextFont { font_size: px(28) }
        )]
    }
}

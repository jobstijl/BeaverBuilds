//! The colony chronicle: a long-lived async task (the "scribe") that wakes
//! each dawn, bridges into the world at the post-reactive sync point to
//! read settled state — including the async drought forecast — and writes
//! the day's entry. The reactive UI renders the entries like any other
//! state: async task → bridge → ECS → reactor, full circle.

use bevy::prelude::*;
use bevy::tasks::AsyncComputeTaskPool;
use bevy_reactive_bsn::AsyncValue;

use crate::bridge::WorldBridge;
use crate::sim::{Population, Season, Stockpile};

#[derive(Resource, Default)]
pub struct Chronicle(pub Vec<String>);

pub struct ChroniclePlugin;

impl Plugin for ChroniclePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Chronicle>()
            .add_systems(Startup, start_scribe)
            .add_systems(Update, feed_days);
    }
}

#[derive(Resource)]
struct DayFeed(async_channel::Sender<u32>);

fn start_scribe(mut commands: Commands, bridge: Res<WorldBridge>) {
    let (tx, rx) = async_channel::unbounded::<u32>();
    commands.insert_resource(DayFeed(tx));
    let bridge = bridge.clone();
    AsyncComputeTaskPool::get()
        .spawn(async move {
            while let Ok(day) = rx.recv().await {
                // Bridge in after reactors have converged: the snapshot is
                // the same settled state the player sees this frame.
                let Some(line) = bridge
                    .run(move |world: &mut World| {
                        let population = world.resource::<Population>().count;
                        let stock = world.resource::<Stockpile>();
                        let (logs, food) = (stock.logs as i64, stock.food as i64);
                        let drought = world.resource::<Season>().drought;
                        let forecast = world
                            .query::<&AsyncValue<f32>>()
                            .iter(world)
                            .find_map(|v| v.ready().copied());
                        let mut line =
                            format!("Day {day}: {population} beavers · {logs} logs · {food} food");
                        if drought {
                            line.push_str(" · enduring drought");
                        } else if let Some(retention) = forecast {
                            line.push_str(&format!(" · drought outlook {:.0}%", retention * 100.0));
                        }
                        line
                    })
                    .await
                else {
                    break;
                };
                if bridge
                    .run(move |world: &mut World| {
                        world.resource_mut::<Chronicle>().0.push(line);
                    })
                    .await
                    .is_none()
                {
                    break;
                }
            }
        })
        .detach();
}

fn feed_days(season: Res<Season>, feed: Res<DayFeed>, mut last: Local<u32>) {
    if season.day != *last {
        *last = season.day;
        if season.day > 1 {
            let _ = feed.0.try_send(season.day);
        }
    }
}

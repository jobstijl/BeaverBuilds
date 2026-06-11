//! A minimal sync-point bridge: async tasks request `&mut World` access at a
//! fixed point in the schedule and await the result.
//!
//! This is a deliberately tiny stand-in for the pattern of Bevy's in-flight
//! `bevy_async` primitive (PR #21744) — enough to demonstrate the
//! composition this project's design report claims: placing the sync point
//! *after* [`ReactSet`](bevy_reactive_bsn::ReactSet) lets async tasks read
//! **settled** post-reactive state in the same frame, and anything they
//! write flows back into the reactive UI like any other ECS state. (The
//! real bevy_async couldn't be used yet: its staging crate pins Bevy 0.18
//! and its repo tracks bevy main.)

use async_channel::{Receiver, Sender};
use bevy::prelude::*;

type Job = Box<dyn FnOnce(&mut World) + Send>;

/// Cheap-to-clone handle for async tasks to bridge into the world.
#[derive(Resource, Clone)]
pub struct WorldBridge {
    tx: Sender<Job>,
}

impl WorldBridge {
    /// Run `f` with full world access at the next sync point; awaits the
    /// returned value.
    pub async fn run<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut World) -> R + Send + 'static,
    ) -> Option<R> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        self.tx
            .send(Box::new(move |world: &mut World| {
                let _ = reply_tx.try_send(f(world));
            }))
            .await
            .ok()?;
        reply_rx.recv().await.ok()
    }
}

/// Queue side held by the drain system.
#[derive(Resource)]
pub struct BridgeQueue(Receiver<Job>);

pub struct BridgePlugin;

impl Plugin for BridgePlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = async_channel::unbounded();
        app.insert_resource(WorldBridge { tx })
            .insert_resource(BridgeQueue(rx))
            // The sync point: after reactors have converged, so bridged
            // closures observe settled state.
            .add_systems(Update, drain_bridge.after(bevy_reactive_bsn::ReactSet));
    }
}

fn drain_bridge(world: &mut World) {
    loop {
        let job = {
            let queue = world.resource::<BridgeQueue>();
            match queue.0.try_recv() {
                Ok(job) => job,
                Err(_) => return,
            }
        };
        job(world);
    }
}

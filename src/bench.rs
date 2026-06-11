//! Headless micro-benchmarks for the reactive layer (`BB_BENCH=1`).
//!
//! Each scenario builds a minimal app (no window/render), warms up, then
//! reports mean frame time over a measured run. The baseline scenarios do the
//! same work with a hand-written `Changed<T>` system — the floor the ECS
//! gives you for free — so the reactive layer's overhead is visible directly.

use std::time::Instant;

use bevy::prelude::*;

use bevy_reactive_bsn::{Dep, ReactSet, ReactiveBsnPlugin, Reactor, ReactorSpec};

#[derive(Component, Clone, Default)]
struct Value(u64);

#[derive(Component, Clone, Default)]
struct Label(u64);

/// How many of the `Value` entities get written each frame.
#[derive(Resource, Clone, Copy)]
struct DirtyPerFrame(usize);

const FRAMES: u32 = 300;
const WARMUP: u32 = 50;

pub fn run_benchmarks() {
    println!("reactive BSN micro-benchmarks ({FRAMES} frames per scenario)");
    println!("{:<58} {:>12} {:>14}", "scenario", "ms/frame", "ns/reactor");
    println!("{}", "-".repeat(86));

    // 1. Idle cost of per-entity deps: nothing changes, reactors only pay
    //    their dirty checks.
    let n = 10_000;
    let ms = bench_reactors(n, 0);
    row("10k patch reactors (Dep::this), all idle", ms, n);

    // 2. Sparse updates: 1% of entities written per frame.
    let ms = bench_reactors(n, 100);
    row(
        "10k patch reactors, 100 dirty/frame (writes + re-apply)",
        ms,
        n,
    );

    // 3. Full churn: every entity written every frame → 10k scene
    //    re-applications per frame. This is the BSN apply throughput.
    let ms = bench_reactors(n, n);
    row("10k patch reactors, all dirty every frame", ms, n);

    // 4. Shared scans: 1k reactors all watching the same component type
    //    across 10k entities. Cost should be ~one scan, not 1000.
    let ms = bench_watchers(10_000, 1_000);
    row(
        "1k Dep::components watchers over 10k entities, idle",
        ms,
        1_000,
    );

    // 5. Value projections: the resource ticks every frame but the
    //    projected value never changes — measures the per-field-wake cost.
    let ms = bench_value_projections(n);
    row(
        "10k value-projection deps, noisy resource, stable value",
        ms,
        n,
    );

    // 6/7. Baseline: identical update work as a plain Changed<T> system.
    let ms = bench_baseline(n, 0);
    row(
        "baseline: plain Changed<T> system, 10k entities, idle",
        ms,
        n,
    );
    let ms = bench_baseline(n, 100);
    row("baseline: plain Changed<T> system, 100 dirty/frame", ms, n);
    let ms = bench_baseline(n, n);
    row("baseline: plain Changed<T> system, all dirty", ms, n);
}

fn row(name: &str, ms: f64, units: usize) {
    println!(
        "{:<58} {:>12.3} {:>14.0}",
        name,
        ms,
        ms * 1.0e6 / units as f64
    );
}

fn base_app(dirty: usize) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        // apply_scene resolves through the asset server (ScenePatch assets).
        .add_plugins(bevy::asset::AssetPlugin::default())
        .add_plugins(bevy::scene::ScenePlugin)
        .insert_resource(DirtyPerFrame(dirty))
        .add_systems(Update, write_values.before(ReactSet));
    app
}

fn write_values(dirty: Res<DirtyPerFrame>, mut values: Query<&mut Value>) {
    for mut value in values.iter_mut().take(dirty.0) {
        value.0 = value.0.wrapping_add(1);
    }
}

fn measure(app: &mut App) -> f64 {
    for _ in 0..WARMUP {
        app.update();
    }
    let start = Instant::now();
    for _ in 0..FRAMES {
        app.update();
    }
    start.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64
}

/// N entities, each carrying a patch reactor mirroring Value into Label.
fn bench_reactors(n: usize, dirty: usize) -> f64 {
    let mut app = base_app(dirty);
    app.add_plugins(ReactiveBsnPlugin);
    let spec = ReactorSpec::patch([Dep::this::<Value>()], |world: &World, entity: Entity| {
        let v = world.get::<Value>(entity).map(|v| v.0).unwrap_or(0);
        bsn! { Label({ v }) }
    });
    for _ in 0..n {
        app.world_mut()
            .spawn((Value(0), Reactor::from_spec(spec.clone())));
    }
    measure(&mut app)
}

/// `entities` plain Value entities plus `watchers` reactors that all share a
/// whole-world `Dep::components::<Value>()` dependency.
fn bench_watchers(entities: usize, watchers: usize) -> f64 {
    let mut app = base_app(0);
    app.add_plugins(ReactiveBsnPlugin);
    for _ in 0..entities {
        app.world_mut().spawn(Value(0));
    }
    let spec = ReactorSpec::patch([Dep::components::<Value>()], |_: &World, _: Entity| {
        bsn! { Label(0) }
    });
    for _ in 0..watchers {
        app.world_mut().spawn(Reactor::from_spec(spec.clone()));
    }
    measure(&mut app)
}

#[derive(Resource, Default)]
struct Noisy(u64);

/// N reactors with a tick-gated projection dep over a resource that is
/// written every frame while the projection stays constant: the worst case
/// for projection cost, the best case for avoided re-renders.
fn bench_value_projections(n: usize) -> f64 {
    let mut app = base_app(0);
    app.add_plugins(ReactiveBsnPlugin)
        .init_resource::<Noisy>()
        .add_systems(
            Update,
            (|mut noisy: ResMut<Noisy>| noisy.0 = noisy.0.wrapping_add(1)).before(ReactSet),
        );
    let spec = ReactorSpec::patch(
        [Dep::resource_value(|noisy: &Noisy| noisy.0 / u64::MAX)],
        |_: &World, _: Entity| bsn! { Label(0) },
    );
    for _ in 0..n {
        app.world_mut().spawn(Reactor::from_spec(spec.clone()));
    }
    measure(&mut app)
}

/// The same Value→Label mirroring as `bench_reactors`, hand-written.
fn bench_baseline(n: usize, dirty: usize) -> f64 {
    let mut app = base_app(dirty);
    app.add_systems(
        Update,
        |mut q: Query<(&Value, &mut Label), Changed<Value>>| {
            for (value, mut label) in &mut q {
                label.0 = value.0;
            }
        },
    );
    for _ in 0..n {
        app.world_mut().spawn((Value(0), Label(0)));
    }
    measure(&mut app)
}

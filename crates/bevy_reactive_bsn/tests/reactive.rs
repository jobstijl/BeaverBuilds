//! Behavioral tests for the reactive layer, run headless (no window/render).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy::prelude::*;
use bevy_reactive_bsn::{
    Dep, ReactiveBsnPlugin, Reactor, ReactorList, ReactorSpec, keyed, reactive,
};

#[derive(Component, Clone, Default, PartialEq, Debug)]
struct Value(u32);

#[derive(Component, Clone, Default, PartialEq, Debug)]
struct Label(u32);

#[derive(Resource, Default)]
struct Score(u32);

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        // apply_scene resolves through the asset server (ScenePatch assets).
        .add_plugins(bevy::asset::AssetPlugin::default())
        .add_plugins(bevy::scene::ScenePlugin)
        .add_plugins(ReactiveBsnPlugin)
        .init_resource::<Score>();
    app
}

/// A run counter that reactors can bump from their scene function.
fn counter() -> (Arc<AtomicUsize>, impl Fn() -> usize) {
    let counter = Arc::new(AtomicUsize::new(0));
    let reader = {
        let counter = counter.clone();
        move || counter.load(Ordering::SeqCst)
    };
    (counter, reader)
}

#[test]
fn patch_reactor_runs_once_then_only_on_change() {
    let mut app = app();
    let (runs, run_count) = counter();
    let reactor = Reactor::patch([Dep::resource::<Score>()], move |world: &World, _| {
        runs.fetch_add(1, Ordering::SeqCst);
        let score = world.resource::<Score>().0;
        bsn! { Label({ score }) }
    });
    let entity = app.world_mut().spawn(reactor).id();

    app.update();
    assert_eq!(run_count(), 1, "first run happens without any change");
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(0)));

    app.update();
    app.update();
    assert_eq!(run_count(), 1, "no spurious wakes while the dep is quiet");

    app.world_mut().resource_mut::<Score>().0 = 7;
    app.update();
    assert_eq!(run_count(), 2, "one wake per change");
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(7)));
}

#[test]
fn patch_merges_in_place_preserving_unrelated_components() {
    let mut app = app();
    let reactor = Reactor::patch([Dep::resource::<Score>()], |world: &World, _| {
        let score = world.resource::<Score>().0;
        bsn! { Label({ score }) }
    });
    let entity = app.world_mut().spawn((reactor, Value(42))).id();
    app.update();
    app.world_mut().resource_mut::<Score>().0 = 1;
    app.update();
    assert_eq!(
        app.world().get::<Value>(entity),
        Some(&Value(42)),
        "re-applying the patch must not disturb unrelated components"
    );
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(1)));
}

#[test]
fn this_dep_tracks_own_component_including_insert_remove() {
    let mut app = app();
    let (runs, run_count) = counter();
    let reactor = Reactor::patch([Dep::this::<Value>()], move |world: &World, e| {
        runs.fetch_add(1, Ordering::SeqCst);
        let v = world.get::<Value>(e).map(|v| v.0).unwrap_or(999);
        bsn! { Label({ v }) }
    });
    let entity = app.world_mut().spawn(reactor).id();
    app.update();
    assert_eq!(run_count(), 1);

    app.world_mut().entity_mut(entity).insert(Value(3));
    app.update();
    assert_eq!(run_count(), 2, "insert wakes");
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(3)));

    app.world_mut()
        .entity_mut(entity)
        .get_mut::<Value>()
        .unwrap()
        .0 = 4;
    app.update();
    assert_eq!(run_count(), 3, "mutation wakes");

    app.world_mut().entity_mut(entity).remove::<Value>();
    app.update();
    assert_eq!(run_count(), 4, "removal wakes");
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(999)));
}

#[test]
fn presence_dep_ignores_mutations() {
    let mut app = app();
    let (runs, run_count) = counter();
    let reactor = Reactor::patch([Dep::presence_this::<Value>()], move |_: &World, _| {
        runs.fetch_add(1, Ordering::SeqCst);
        bsn! { Label(0) }
    });
    let entity = app.world_mut().spawn((reactor, Value(0))).id();
    app.update();
    assert_eq!(run_count(), 1);

    app.world_mut()
        .entity_mut(entity)
        .get_mut::<Value>()
        .unwrap()
        .0 = 5;
    app.update();
    assert_eq!(run_count(), 1, "mutations must not wake a presence dep");

    app.world_mut().entity_mut(entity).remove::<Value>();
    app.update();
    assert_eq!(run_count(), 2, "removal wakes");

    app.world_mut().entity_mut(entity).insert(Value(1));
    app.update();
    assert_eq!(run_count(), 3, "re-insert wakes");
}

#[test]
fn components_dep_wakes_on_any_entity_and_population_changes() {
    let mut app = app();
    let (runs, run_count) = counter();
    let reactor = Reactor::patch([Dep::components::<Value>()], move |_: &World, _| {
        runs.fetch_add(1, Ordering::SeqCst);
        bsn! { Label(0) }
    });
    app.world_mut().spawn(reactor);
    app.update();
    assert_eq!(run_count(), 1);

    let other = app.world_mut().spawn(Value(1)).id();
    app.update();
    assert_eq!(run_count(), 2, "new entity with the component wakes");

    app.world_mut()
        .entity_mut(other)
        .get_mut::<Value>()
        .unwrap()
        .0 = 2;
    app.update();
    assert_eq!(run_count(), 3, "mutation on any entity wakes");

    app.update();
    assert_eq!(run_count(), 3, "quiet world, quiet reactor");

    app.world_mut().entity_mut(other).despawn();
    app.update();
    assert_eq!(run_count(), 4, "despawn wakes (population change)");
}

#[test]
fn rebuild_replaces_children() {
    let mut app = app();
    let reactor = Reactor::rebuild([Dep::resource::<Score>()], |world: &World, _| {
        let n = world.resource::<Score>().0;
        let children: Vec<_> = (0..n)
            .map(|i| Box::new(bsn! { Value({ i }) }) as Box<dyn bevy::scene::Scene>)
            .collect();
        bsn! { Children [ { children } ] }
    });
    let entity = app.world_mut().spawn(reactor).id();
    app.world_mut().resource_mut::<Score>().0 = 3;
    app.update();
    let first: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(first.len(), 3);

    app.world_mut().resource_mut::<Score>().0 = 1;
    app.update();
    let second: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(second.len(), 1);
    for old in first {
        assert!(
            app.world().get_entity(old).is_err(),
            "rebuild must despawn the previous subtree"
        );
    }
}

#[test]
fn list_reconciles_membership_and_leaves_survivors_untouched() {
    let mut app = app();
    let list = ReactorList::new([Dep::resource::<Score>()], |world: &World| {
        let score = world.resource::<Score>().0;
        let mut items = Vec::new();
        for key in 0..32u32 {
            if score & (1 << key) != 0 {
                items.push(keyed(key as u64, bsn! { Value({ key }) }));
            }
        }
        items
    });
    let entity = app.world_mut().spawn(list).id();
    app.world_mut().resource_mut::<Score>().0 = 0b01; // key 0
    app.update();
    let children: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(children.len(), 1);
    let survivor = children[0];

    app.world_mut().resource_mut::<Score>().0 = 0b11; // keys 0, 1
    app.update();
    let children: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(children.len(), 2);
    assert_eq!(
        children[0], survivor,
        "surviving key must keep its entity (membership-only reconciliation)"
    );

    app.world_mut().resource_mut::<Score>().0 = 0b10; // key 1 only
    app.update();
    let children: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(children.len(), 1);
    assert!(
        app.world().get_entity(survivor).is_err(),
        "vanished key must despawn its child"
    );
}

#[test]
fn reactor_chains_settle_within_one_frame() {
    let mut app = app();
    // A patches Value on its own entity from Score.
    let a = app
        .world_mut()
        .spawn(Reactor::patch(
            [Dep::resource::<Score>()],
            |world: &World, _| {
                let score = world.resource::<Score>().0;
                bsn! { Value({ score }) }
            },
        ))
        .id();
    // B watches A's Value — its wake depends on a write made *by a reactor*,
    // which also exercises the written-entity pass filter.
    let b = app
        .world_mut()
        .spawn(Reactor::patch(
            [Dep::entity::<Value>(a)],
            move |world: &World, _| {
                let v = world.get::<Value>(a).map(|v| v.0).unwrap_or(0);
                bsn! { Label({ v }) }
            },
        ))
        .id();
    app.update();
    app.world_mut().resource_mut::<Score>().0 = 9;
    app.update();
    assert_eq!(
        app.world().get::<Label>(b),
        Some(&Label(9)),
        "the chain A -> B must settle in a single frame"
    );
}

#[test]
fn inline_reactive_composes_in_bsn_and_forks_per_spawn() {
    use bevy::scene::WorldSceneExt;
    let mut app = app();
    fn fragment() -> impl bevy::scene::Scene {
        bsn! {
            Value(1)
            Children [
                reactive([Dep::resource::<Score>()], |world: &World, _: Entity| {
                    let score = world.resource::<Score>().0;
                    bsn! { Label({ score }) }
                }),
            ]
        }
    }
    let one = app.world_mut().spawn_scene(fragment()).unwrap().id();
    let two = app.world_mut().spawn_scene(fragment()).unwrap().id();
    app.world_mut().resource_mut::<Score>().0 = 5;
    app.update();
    for root in [one, two] {
        let children: Vec<Entity> = app.world().get::<Children>(root).unwrap().iter().collect();
        assert_eq!(children.len(), 1);
        assert_eq!(
            app.world().get::<Label>(children[0]),
            Some(&Label(5)),
            "each spawn forks its own reactor instance"
        );
    }
}

#[test]
fn shared_spec_forks_independent_instances() {
    let mut app = app();
    let spec = ReactorSpec::patch([Dep::this::<Value>()], |world: &World, e| {
        let v = world.get::<Value>(e).map(|v| v.0).unwrap_or(0);
        bsn! { Label({ v }) }
    });
    let a = app
        .world_mut()
        .spawn((Value(1), Reactor::from_spec(spec.clone())))
        .id();
    let b = app
        .world_mut()
        .spawn((Value(2), Reactor::from_spec(spec)))
        .id();
    app.update();
    assert_eq!(app.world().get::<Label>(a), Some(&Label(1)));
    assert_eq!(app.world().get::<Label>(b), Some(&Label(2)));

    // Waking one instance must not wake the other.
    app.world_mut().entity_mut(a).get_mut::<Value>().unwrap().0 = 10;
    app.update();
    assert_eq!(app.world().get::<Label>(a), Some(&Label(10)));
    assert_eq!(app.world().get::<Label>(b), Some(&Label(2)));
}

#[test]
fn despawning_a_reactor_entity_is_clean_teardown() {
    let mut app = app();
    let entity = app
        .world_mut()
        .spawn(Reactor::patch(
            [Dep::resource::<Score>()],
            |_: &World, _| bsn! { Label(0) },
        ))
        .id();
    app.update();
    app.world_mut().entity_mut(entity).despawn();
    app.world_mut().resource_mut::<Score>().0 = 1;
    app.update();
    app.update();
}

#[test]
fn self_dirtying_reactor_terminates_each_frame() {
    let mut app = app();
    // Pathological: the reactor writes its own dependency on every run.
    // It must hit the pass cap and yield (with a warning), not hang.
    let entity = app
        .world_mut()
        .spawn((
            Value(0),
            Reactor::patch([Dep::this::<Value>()], |world: &World, e| {
                let v = world.get::<Value>(e).map(|v| v.0).unwrap_or(0);
                bsn! { Value({ v + 1 }) }
            }),
        ))
        .id();
    app.update();
    app.update();
    assert!(app.world().get::<Value>(entity).unwrap().0 > 0);
}

// ---------------------------------------------------------------------------
// Async resources
// ---------------------------------------------------------------------------

use bevy_reactive_bsn::{AsyncView, reactive_async};

fn async_child(app: &App, root: Entity) -> Entity {
    let children: Vec<Entity> = app.world().get::<Children>(root).unwrap().iter().collect();
    assert_eq!(children.len(), 1);
    children[0]
}

fn update_until_label(app: &mut App, entity: Entity, expected: u32) {
    for _ in 0..200 {
        app.update();
        if app.world().get::<Label>(entity) == Some(&Label(expected)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!(
        "label never reached {expected}; last = {:?}",
        app.world().get::<Label>(entity)
    );
}

#[test]
fn async_resource_renders_pending_then_ready() {
    use bevy::scene::WorldSceneExt;
    let mut app = app();
    app.world_mut().resource_mut::<Score>().0 = 1;
    let root = app
        .world_mut()
        .spawn_scene(reactive_async(
            [Dep::resource::<Score>()],
            |world: &World, _| {
                let score = world.resource::<Score>().0;
                async move { score + 100 }
            },
            |_: &World, _, view: AsyncView<u32>| match view.ready() {
                None => Box::new(bsn! { Label(0) }) as Box<dyn bevy::scene::Scene>,
                Some(&n) => Box::new(bsn! { Label({ n }) }) as Box<dyn bevy::scene::Scene>,
            },
        ))
        .unwrap()
        .id();
    app.update();
    let child = async_child(&app, root);
    // First frame: the fallback (the task can't have been driven yet).
    assert_eq!(app.world().get::<Label>(child), Some(&Label(0)));
    // Then the result arrives and the child re-renders.
    update_until_label(&mut app, child, 101);
}

#[test]
fn async_resource_keeps_stale_value_while_revalidating() {
    use std::pin::Pin;

    use bevy::scene::WorldSceneExt;
    let mut app = app();
    app.world_mut().resource_mut::<Score>().0 = 1;
    let root = app
        .world_mut()
        .spawn_scene(reactive_async(
            [Dep::resource::<Score>()],
            |world: &World, _| -> Pin<Box<dyn Future<Output = u32> + Send>> {
                let score = world.resource::<Score>().0;
                if score == 1 {
                    Box::pin(async move { score })
                } else {
                    // The recomputation never finishes.
                    Box::pin(std::future::pending())
                }
            },
            |_: &World, _, view: AsyncView<u32>| {
                // Encode both facts in one readout: stale value + 100 while
                // a recomputation is in flight.
                let shown =
                    view.ready().copied().unwrap_or(0) + if view.refreshing { 100 } else { 0 };
                bsn! { Label({ shown }) }
            },
        ))
        .unwrap()
        .id();
    app.update();
    let child = async_child(&app, root);
    update_until_label(&mut app, child, 1);

    // Dependency changes; the new computation hangs forever. The old value
    // must keep rendering (last good value), and the view must report the
    // refresh so it can be marked stale.
    app.world_mut().resource_mut::<Score>().0 = 2;
    for _ in 0..10 {
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(
        app.world().get::<Label>(child),
        Some(&Label(101)),
        "old Ready value must persist AND be reported as refreshing"
    );
}

// ---------------------------------------------------------------------------
// Write-contract violation: graceful degradation
// ---------------------------------------------------------------------------

/// A reactor that breaks the write contract (writing a watched component on
/// an unrelated entity from inside its scene) must degrade to a ONE-FRAME
/// delay for the watcher — never a lost update — because pass 0 of the next
/// frame checks every reactor unconditionally.
#[test]
fn contract_violation_degrades_to_one_frame_delay() {
    let mut app = app();
    let other = app.world_mut().spawn(Value(0)).id();

    // The watcher is spawned FIRST so the violator's same-pass write happens
    // after the watcher was already checked (worst-case ordering).
    let watcher = app
        .world_mut()
        .spawn(Reactor::patch(
            [Dep::entity::<Value>(other)],
            move |world: &World, _| {
                let v = world.get::<Value>(other).map(|v| v.0).unwrap_or(0);
                bsn! { Label({ v }) }
            },
        ))
        .id();

    // The violator writes `other` (outside its subtree) from a template.
    // Debug builds will log a loud contract-violation error for this — that
    // is the enforcement working, and exactly what this test simulates.
    let violator = Reactor::patch([Dep::resource::<Score>()], move |_: &World, _| {
        bevy::ecs::template::template(move |ctx| {
            let score = ctx.resource::<Score>().0;
            ctx.entity.world_scope(|world| {
                if let Ok(mut e) = world.get_entity_mut(other) {
                    e.insert(Value(score));
                }
            });
            Ok(Label(0))
        })
    });
    app.world_mut().spawn(violator);

    app.update(); // first runs; watcher settles on Value(0)
    assert_eq!(app.world().get::<Label>(watcher), Some(&Label(0)));

    app.world_mut().resource_mut::<Score>().0 = 9;
    app.update();
    // Same frame: the watcher was checked before the violator wrote, and the
    // pass filter (soundly, per its contract) skipped re-checking it.
    // It may be stale now — but never longer than this frame:
    app.update();
    assert_eq!(
        app.world().get::<Label>(watcher),
        Some(&Label(9)),
        "a missed wake must be repaired by the next frame's unconditional pass"
    );
}

/// `Dep::parent` implicitly depends on the `ChildOf` edge: re-parenting must
/// wake the dep even when neither parent's watched component ticked.
#[test]
fn parent_dep_wakes_on_reparent() {
    let mut app = app();
    let parent_a = app.world_mut().spawn(Value(1)).id();
    let parent_b = app.world_mut().spawn(Value(2)).id();
    let child = app
        .world_mut()
        .spawn((
            ChildOf(parent_a),
            Reactor::patch([Dep::parent::<Value>()], |world: &World, child| {
                let v = world
                    .get::<ChildOf>(child)
                    .and_then(|c| world.get::<Value>(c.parent()))
                    .map(|v| v.0)
                    .unwrap_or(0);
                bsn! { Label({ v }) }
            }),
        ))
        .id();
    app.update();
    app.update();
    assert_eq!(app.world().get::<Label>(child), Some(&Label(1)));

    // Pure re-parent: both parents' `Value` ticks stay untouched.
    app.world_mut().entity_mut(child).insert(ChildOf(parent_b));
    app.update();
    assert_eq!(
        app.world().get::<Label>(child),
        Some(&Label(2)),
        "re-parenting must wake a Dep::parent watcher"
    );
}

// ---------------------------------------------------------------------------
// Coverage sweep: every dep type, every edge found by auditing
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct Flag(#[allow(dead_code)] u32);

#[test]
fn resource_dep_wakes_on_insert_and_remove() {
    let mut app = app();
    let (runs, run_count) = counter();
    app.world_mut().spawn(Reactor::patch(
        [Dep::resource::<Flag>()],
        move |_: &World, _| {
            runs.fetch_add(1, Ordering::SeqCst);
            bsn! { Label(0) }
        },
    ));
    app.update();
    assert_eq!(run_count(), 1, "first run with the resource absent");

    app.world_mut().insert_resource(Flag(1));
    app.update();
    assert_eq!(run_count(), 2, "resource insertion wakes");

    app.update();
    assert_eq!(run_count(), 2);

    app.world_mut().remove_resource::<Flag>();
    app.update();
    assert_eq!(run_count(), 3, "resource removal wakes");
}

#[test]
fn entity_dep_wakes_when_target_despawns() {
    let mut app = app();
    let (runs, run_count) = counter();
    let target = app.world_mut().spawn(Value(1)).id();
    app.world_mut().spawn(Reactor::patch(
        [Dep::entity::<Value>(target)],
        move |_: &World, _| {
            runs.fetch_add(1, Ordering::SeqCst);
            bsn! { Label(0) }
        },
    ));
    app.update();
    assert_eq!(run_count(), 1);

    app.world_mut().entity_mut(target).despawn();
    app.update();
    assert_eq!(run_count(), 2, "watched entity despawn wakes");
    app.update();
    assert_eq!(run_count(), 2, "and only once");
}

#[test]
fn parent_dep_wakes_when_orphaned() {
    let mut app = app();
    let parent = app.world_mut().spawn(Value(7)).id();
    let child = app
        .world_mut()
        .spawn((
            ChildOf(parent),
            Reactor::patch([Dep::parent::<Value>()], |world: &World, child| {
                let v = world
                    .get::<ChildOf>(child)
                    .and_then(|c| world.get::<Value>(c.parent()))
                    .map(|v| v.0)
                    .unwrap_or(0);
                bsn! { Label({ v }) }
            }),
        ))
        .id();
    app.update();
    assert_eq!(app.world().get::<Label>(child), Some(&Label(7)));

    app.world_mut().entity_mut(child).remove::<ChildOf>();
    app.update();
    assert_eq!(
        app.world().get::<Label>(child),
        Some(&Label(0)),
        "losing the parent edge must wake the dep"
    );
}

#[test]
fn related_components_dep_tracks_members_and_membership() {
    let mut app = app();
    let (runs, run_count) = counter();
    let parent = app.world_mut().spawn_empty().id();
    let member_a = app.world_mut().spawn((Value(1), ChildOf(parent))).id();
    app.world_mut().spawn(Reactor::patch(
        [Dep::related_components::<Children, Value>(parent)],
        move |_: &World, _| {
            runs.fetch_add(1, Ordering::SeqCst);
            bsn! { Label(0) }
        },
    ));
    app.update();
    assert_eq!(run_count(), 1);

    app.world_mut()
        .entity_mut(member_a)
        .get_mut::<Value>()
        .unwrap()
        .0 = 2;
    app.update();
    assert_eq!(run_count(), 2, "member mutation wakes");

    let member_b = app.world_mut().spawn((Value(9), ChildOf(parent))).id();
    app.update();
    assert_eq!(run_count(), 3, "new member wakes");

    app.update();
    assert_eq!(run_count(), 3, "quiet graph, quiet reactor");

    app.world_mut().entity_mut(member_b).remove::<Value>();
    app.update();
    assert_eq!(run_count(), 4, "member losing the component wakes");

    app.world_mut().entity_mut(member_a).despawn();
    app.update();
    assert_eq!(run_count(), 5, "member despawn wakes");
}

#[test]
fn components_filtered_ignores_non_matching_entities() {
    let mut app = app();
    let (runs, run_count) = counter();
    let marked = app.world_mut().spawn((Value(1), Label(0))).id();
    let unmarked = app.world_mut().spawn(Value(1)).id();
    app.world_mut().spawn(Reactor::patch(
        [Dep::components_filtered::<Value, With<Label>>()],
        move |_: &World, _| {
            runs.fetch_add(1, Ordering::SeqCst);
            bsn! { Value(0) }
        },
    ));
    app.update();
    assert_eq!(run_count(), 1);

    app.world_mut()
        .entity_mut(unmarked)
        .get_mut::<Value>()
        .unwrap()
        .0 = 5;
    app.update();
    assert_eq!(run_count(), 1, "changes outside the filter must not wake");

    app.world_mut()
        .entity_mut(marked)
        .get_mut::<Value>()
        .unwrap()
        .0 = 5;
    app.update();
    assert_eq!(run_count(), 2, "changes inside the filter wake");
}

#[test]
fn rebuilt_children_with_nested_reactors_settle_same_frame() {
    let mut app = app();
    let root = app
        .world_mut()
        .spawn(Reactor::rebuild(
            [Dep::resource::<Score>()],
            |_: &World, _| {
                bsn! {
                    Children [
                        reactive([Dep::resource::<Score>()], |world: &World, _: Entity| {
                            let score = world.resource::<Score>().0;
                            bsn! { Label({ score }) }
                        }),
                    ]
                }
            },
        ))
        .id();
    app.world_mut().resource_mut::<Score>().0 = 4;
    app.update();
    // The rebuild spawned a fresh child whose own (brand-new) reactor must
    // have run within the same frame's convergence passes.
    let children: Vec<Entity> = app.world().get::<Children>(root).unwrap().iter().collect();
    assert_eq!(app.world().get::<Label>(children[0]), Some(&Label(4)));
}

#[test]
fn list_reorders_surviving_children() {
    let mut app = app();
    let list = ReactorList::new([Dep::resource::<Score>()], |world: &World| {
        let flipped = world.resource::<Score>().0 != 0;
        let keys: [u64; 2] = if flipped { [2, 1] } else { [1, 2] };
        keys.into_iter()
            .map(|k| keyed(k, bsn! { Value({ k as u32 }) }))
            .collect()
    });
    let entity = app.world_mut().spawn(list).id();
    app.update();
    let before: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(before.len(), 2);

    app.world_mut().resource_mut::<Score>().0 = 1;
    app.update();
    let after: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(
        after,
        vec![before[1], before[0]],
        "same entities, order must follow the key order"
    );
}

#[test]
fn list_duplicate_keys_are_ignored_with_one_child() {
    let mut app = app();
    let list = ReactorList::new([Dep::resource::<Score>()], |_: &World| {
        vec![keyed(1, bsn! { Value(1) }), keyed(1, bsn! { Value(2) })]
    });
    let entity = app.world_mut().spawn(list).id();
    app.update();
    app.world_mut().resource_mut::<Score>().0 = 1;
    app.update();
    let children: Vec<Entity> = app
        .world()
        .get::<Children>(entity)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(children.len(), 1, "duplicate keys collapse to one child");
}

/// Two inline fragments in the *same* `bsn!` entity still fail loudly at
/// spawn (BSN puts two `Reactor` templates into one bundle before any merge
/// can run). Multiple fragments per entity are composed with
/// `Reactor::and(..)`, sequential `apply_scene`s, or child entities.
#[test]
#[should_panic(expected = "duplicate components")]
fn two_inline_reactors_in_one_scene_panic_at_spawn() {
    use bevy::scene::WorldSceneExt;
    let mut app = app();
    let _ = app.world_mut().spawn_scene(bsn! {
        reactive([Dep::resource::<Score>()], |_: &World, _: Entity| {
            bsn! { Value(111) }
        })
        reactive([Dep::resource::<Score>()], |_: &World, _: Entity| {
            bsn! { Label(222) }
        })
    });
}

#[test]
fn async_recompute_replaces_ready_value() {
    use bevy::scene::WorldSceneExt;
    let mut app = app();
    app.world_mut().resource_mut::<Score>().0 = 1;
    let root = app
        .world_mut()
        .spawn_scene(reactive_async(
            [Dep::resource::<Score>()],
            |world: &World, _| {
                let score = world.resource::<Score>().0;
                async move { score }
            },
            |_: &World, _, view: AsyncView<u32>| {
                // Encode both facts in one readout: stale value + 100 while
                // a recomputation is in flight.
                let shown =
                    view.ready().copied().unwrap_or(0) + if view.refreshing { 100 } else { 0 };
                bsn! { Label({ shown }) }
            },
        ))
        .unwrap()
        .id();
    app.update();
    let child = async_child(&app, root);
    update_until_label(&mut app, child, 1);

    app.world_mut().resource_mut::<Score>().0 = 3;
    update_until_label(&mut app, child, 3);
}

// ---------------------------------------------------------------------------
// Value projections: per-field wake granularity
// ---------------------------------------------------------------------------

#[test]
fn resource_value_wakes_only_when_projection_changes() {
    let mut app = app();
    let (runs, run_count) = counter();
    app.world_mut().spawn(Reactor::patch(
        [Dep::resource_value(|s: &Score| s.0 / 10)],
        move |_: &World, _| {
            runs.fetch_add(1, Ordering::SeqCst);
            bsn! { Label(0) }
        },
    ));
    app.update();
    assert_eq!(run_count(), 1);

    app.world_mut().resource_mut::<Score>().0 = 7; // projection still 0
    app.update();
    assert_eq!(run_count(), 1, "resource ticked but projection unchanged");

    app.world_mut().resource_mut::<Score>().0 = 15; // projection now 1
    app.update();
    assert_eq!(run_count(), 2, "projection change wakes");
}

#[test]
fn resource_value_projection_is_tick_gated() {
    let mut app = app();
    let (projections, projection_count) = counter();
    app.world_mut().spawn(Reactor::patch(
        [Dep::resource_value(move |s: &Score| {
            projections.fetch_add(1, Ordering::SeqCst);
            s.0 / 10
        })],
        |_: &World, _| bsn! { Label(0) },
    ));
    app.update();
    let after_first = projection_count();
    assert!(after_first >= 1);

    // Quiet resource: the projection must not run at all.
    app.update();
    app.update();
    assert_eq!(
        projection_count(),
        after_first,
        "quiet resources must cost one tick compare, not a projection"
    );

    // Noisy resource, stable projection: exactly one more projection per
    // change, and (per the previous test) no reactor wake.
    app.world_mut().resource_mut::<Score>().0 = 3;
    app.update();
    assert_eq!(projection_count(), after_first + 1);
}

#[test]
fn this_value_projects_own_component() {
    let mut app = app();
    let (runs, run_count) = counter();
    let entity = app
        .world_mut()
        .spawn((
            Value(0),
            Reactor::patch(
                [Dep::this_value(|v: &Value| v.0 / 10)],
                move |world: &World, e| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    let bucket = world.get::<Value>(e).map(|v| v.0 / 10).unwrap_or(0);
                    bsn! { Label({ bucket }) }
                },
            ),
        ))
        .id();
    app.update();
    assert_eq!(run_count(), 1);

    app.world_mut()
        .entity_mut(entity)
        .get_mut::<Value>()
        .unwrap()
        .0 = 9;
    app.update();
    assert_eq!(run_count(), 1, "same bucket, no wake");

    app.world_mut()
        .entity_mut(entity)
        .get_mut::<Value>()
        .unwrap()
        .0 = 25;
    app.update();
    assert_eq!(run_count(), 2);
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(2)));
}

// ---------------------------------------------------------------------------
// Multiple fragments per entity: the re-application half of per-field
// ---------------------------------------------------------------------------

/// Two fragments on ONE entity with distinct projections: each wakes — and
/// re-applies its own small patch — independently. This is per-field
/// re-application by fragment splitting.
#[test]
fn and_composes_independent_fragments_on_one_entity() {
    let mut app = app();
    let (runs_a, count_a) = counter();
    let (runs_b, count_b) = counter();
    let entity = app
        .world_mut()
        .spawn(
            Reactor::patch(
                [Dep::resource_value(|s: &Score| s.0 / 10)],
                move |world: &World, _| {
                    runs_a.fetch_add(1, Ordering::SeqCst);
                    let bucket = world.resource::<Score>().0 / 10;
                    bsn! { Value({ bucket }) }
                },
            )
            .and(bevy_reactive_bsn::ReactorSpec::patch(
                [Dep::resource_value(|s: &Score| s.0 % 2)],
                move |world: &World, _| {
                    runs_b.fetch_add(1, Ordering::SeqCst);
                    let parity = world.resource::<Score>().0 % 2;
                    bsn! { Label({ parity }) }
                },
            )),
        )
        .id();
    app.update();
    assert_eq!((count_a(), count_b()), (1, 1));
    assert_eq!(app.world().get::<Value>(entity), Some(&Value(0)));
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(0)));

    // Parity flips, bucket doesn't: only fragment B re-applies.
    app.world_mut().resource_mut::<Score>().0 = 1;
    app.update();
    assert_eq!(
        (count_a(), count_b()),
        (1, 2),
        "only the parity fragment wakes"
    );
    assert_eq!(app.world().get::<Label>(entity), Some(&Label(1)));

    // Bucket flips, parity doesn't (1 -> 11): only fragment A re-applies.
    app.world_mut().resource_mut::<Score>().0 = 11;
    app.update();
    assert_eq!(
        (count_a(), count_b()),
        (2, 2),
        "only the bucket fragment wakes"
    );
    assert_eq!(app.world().get::<Value>(entity), Some(&Value(1)));
}

/// Applying a second scene with an inline fragment MERGES it next to the
/// first instead of clobbering it. Merge identity is the spec's `Arc`:
/// internal re-renders re-apply the *same* spec and replace in place
/// (idempotent), while a *reconstructed* scene is a new spec and appends —
/// so repeatedly applying rebuilt fragment scenes to a long-lived entity
/// accumulates fragments (documented behavior; structural paths are safe
/// because rebuilds despawn child entities wholesale).
#[test]
fn sequential_scene_applications_merge_inline_fragments() {
    use bevy::scene::{EntityWorldMutSceneExt, WorldSceneExt};
    let mut app = app();
    let (runs_a, count_a) = counter();
    let (runs_b, count_b) = counter();
    let fragment_a = move || {
        let runs_a = runs_a.clone();
        reactive(
            [Dep::resource::<Score>()],
            move |world: &World, _: Entity| {
                runs_a.fetch_add(1, Ordering::SeqCst);
                let score = world.resource::<Score>().0;
                bsn! { Value({ score }) }
            },
        )
    };
    let entity = app.world_mut().spawn_scene(fragment_a()).unwrap().id();

    let fragment_b = reactive([Dep::resource::<Score>()], move |_: &World, _: Entity| {
        runs_b.fetch_add(1, Ordering::SeqCst);
        bsn! { Label(7) }
    });
    app.world_mut()
        .entity_mut(entity)
        .apply_scene(fragment_b)
        .unwrap();
    app.update();
    assert_eq!(
        app.world().get::<Value>(entity),
        Some(&Value(0)),
        "fragment A survives"
    );
    assert_eq!(
        app.world().get::<Label>(entity),
        Some(&Label(7)),
        "fragment B merged in"
    );
    assert_eq!((count_a(), count_b()), (1, 1));

    // Re-applying a RECONSTRUCTED A-scene appends a second A instance (new
    // spec Arc — append semantics), and never clobbers B.
    app.world_mut()
        .entity_mut(entity)
        .apply_scene(fragment_a())
        .unwrap();
    app.world_mut().resource_mut::<Score>().0 = 5;
    app.update();
    assert_eq!(app.world().get::<Value>(entity), Some(&Value(5)));
    assert_eq!(count_b(), 2, "B: one instance, one wake per change");
    assert_eq!(
        count_a(),
        1 + 2,
        "A: reconstructed scene appended a second instance (both wake once)"
    );
}

/// A rebuild that despawns children carrying their own (dirty) reactors must
/// not panic when the runner reaches the despawned children's entries later
/// in the same pass. (Regression: this was archetype-order dependent.)
#[test]
fn rebuild_despawning_reactive_children_same_pass_is_safe() {
    let mut app = app();
    let root = app
        .world_mut()
        .spawn(Reactor::rebuild(
            [Dep::resource::<Score>()],
            |_: &World, _| {
                bsn! {
                    Children [
                        reactive([Dep::resource::<Score>()], |world: &World, _: Entity| {
                            let score = world.resource::<Score>().0;
                            bsn! { Label({ score }) }
                        }),
                    ]
                }
            },
        ))
        .id();
    app.update();
    // Both the root (rebuild) and the child's reactor are dirty in the same
    // pass; the rebuild despawns the child before its entry is reached.
    app.world_mut().resource_mut::<Score>().0 = 5;
    app.update();
    let children: Vec<Entity> = app.world().get::<Children>(root).unwrap().iter().collect();
    assert_eq!(app.world().get::<Label>(children[0]), Some(&Label(5)));
}

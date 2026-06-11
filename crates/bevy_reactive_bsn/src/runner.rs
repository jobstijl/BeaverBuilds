//! The reactor runner: one exclusive system that brings every reactor in
//! line with the current world state, looping until nothing is dirty so
//! reactor chains settle within a single frame.
//!
//! Convergence is cheap because of what renders are allowed to write: a
//! reactor's render only touches components on its own target entity and any
//! children it (de)spawns — never resources. The first pass therefore checks
//! everything, while later passes skip every reactor whose entity-targeted
//! dependencies point outside the set of entities written by the previous
//! pass (resource deps are O(1) re-checks; whole-world deps share one scan
//! per type).

use bevy::ecs::change_detection::Tick;
use bevy::ecs::entity::EntityHashSet;
use bevy::prelude::*;

use std::sync::Arc;

use super::dep::{DepCx, DepState, SharedScans, clamp_tick};
use super::{Mode, Reactor, ReactorInstance, ReactorList, ReactorListSpec};

/// Convergence passes allowed per frame before we assume two reactors are
/// re-triggering each other.
const MAX_PASSES: usize = 6;

pub fn run_reactors(world: &mut World, mut scans: Local<SharedScans>, mut frame: Local<u32>) {
    // Entities written during the previous pass; pass 0 checks everything.
    let mut written_prev = EntityHashSet::default();
    // The debug write-contract checker sweeps entity ticks, which is too
    // expensive to run every frame at scale — so it samples. Realistic
    // violations are systematic (a misbehaving reactor misbehaves on every
    // run), so sampling still catches them within a second.
    #[cfg(debug_assertions)]
    let check_this_frame = {
        *frame = frame.wrapping_add(1);
        frame.is_multiple_of(16)
    };
    #[cfg(not(debug_assertions))]
    let _ = &mut frame;
    #[cfg(debug_assertions)]
    let mut watched_cache: Option<std::collections::HashSet<std::any::TypeId>> = None;

    for pass in 0..MAX_PASSES {
        let this_run = world.change_tick();
        let mut written = EntityHashSet::default();
        let mut ran_any = false;

        // --- Plain reactors -------------------------------------------------
        // Detach instances so deps and renders can take &mut World.
        let mut reactors: Vec<(Entity, Vec<ReactorInstance>)> = Vec::new();
        for (entity, mut reactor) in world.query::<(Entity, &mut Reactor)>().iter_mut(world) {
            let reactor = reactor.bypass_change_detection();
            for instance in &mut reactor.instances {
                clamp_tick(&mut instance.last_run, this_run);
            }
            if pass > 0
                && reactor
                    .instances
                    .iter()
                    .all(|i| skips_pass(&i.spec.deps, i.last_run, &written_prev, entity))
            {
                continue;
            }
            reactors.push((entity, std::mem::take(&mut reactor.instances)));
        }

        for (entity, mut instances) in reactors {
            // An earlier entry this pass may have despawned this entity
            // (e.g. a rebuild tearing down children that carry reactors).
            if world.get_entity(entity).is_err() {
                continue;
            }
            for instance in &mut instances {
                if pass > 0
                    && skips_pass(
                        &instance.spec.deps,
                        instance.last_run,
                        &written_prev,
                        entity,
                    )
                {
                    continue;
                }
                let woke = first_dirty_dep(
                    world,
                    &mut scans,
                    &instance.spec.deps,
                    &mut instance.state,
                    entity,
                    instance.last_run,
                    this_run,
                );
                let Some(index) = woke else { continue };
                ran_any = true;
                debug!(
                    target: "reactive_bsn",
                    "reactor on {entity} woken by {}",
                    instance.spec.deps[index].describe()
                );
                written.insert(entity);
                if instance.spec.mode == Mode::Rebuild {
                    collect_descendants(world, entity, &mut written);
                    world.entity_mut(entity).despawn_related::<Children>();
                }
                if let Err(err) = (instance.spec.render)(world, entity) {
                    warn!("reactor on {entity} failed to apply scene: {err:?}");
                }
                // Changes this instance just made don't re-dirty it.
                instance.last_run = world.change_tick();
            }
            if let Some(mut reactor) = world.get_mut::<Reactor>(entity) {
                let reactor = reactor.bypass_change_detection();
                // A render may have merged NEW instances onto this entity
                // (inline fragments in an applied scene); keep those.
                for added in std::mem::take(&mut reactor.instances) {
                    if !instances
                        .iter()
                        .any(|i| Arc::ptr_eq(&i.spec.render, &added.spec.render))
                    {
                        instances.push(added);
                    }
                }
                reactor.instances = instances;
            }
        }

        // --- Keyed lists -----------------------------------------------------
        type DetachedList = (
            Entity,
            ReactorListSpec,
            Tick,
            Vec<DepState>,
            Vec<(u64, Entity)>,
        );
        let mut lists: Vec<DetachedList> = Vec::new();
        for (entity, mut list) in world.query::<(Entity, &mut ReactorList)>().iter_mut(world) {
            let list = list.bypass_change_detection();
            clamp_tick(&mut list.last_run, this_run);
            if pass > 0 && skips_pass(&list.spec.deps, list.last_run, &written_prev, entity) {
                continue;
            }
            lists.push((
                entity,
                list.spec.clone(),
                list.last_run,
                std::mem::take(&mut list.state),
                std::mem::take(&mut list.spawned),
            ));
        }

        for (entity, spec, last_run, mut state, mut spawned) in lists {
            if world.get_entity(entity).is_err() {
                continue;
            }
            let woke = first_dirty_dep(
                world, &mut scans, &spec.deps, &mut state, entity, last_run, this_run,
            );
            let mut new_last = last_run;
            if let Some(index) = woke {
                ran_any = true;
                debug!(
                    target: "reactive_bsn",
                    "reactor list on {entity} woken by {}",
                    spec.deps[index].describe()
                );
                written.insert(entity);
                let desired = (spec.items)(world);

                // Membership and order only: despawn vanished keys, spawn new
                // ones, leave survivors alone (their content updates itself
                // via embedded reactors).
                for (key, child) in spawned.iter() {
                    if !desired.iter().any(|(k, _)| k == key) {
                        written.insert(*child);
                        collect_descendants(world, *child, &mut written);
                        if let Ok(e) = world.get_entity_mut(*child) {
                            e.despawn();
                        }
                    }
                }
                let mut next: Vec<(u64, Entity)> = Vec::with_capacity(desired.len());
                for (key, scene) in desired {
                    if next.iter().any(|(k, _)| *k == key) {
                        warn!(
                            "reactor list on {entity} produced duplicate key {key}; \
                             ignoring the later duplicate"
                        );
                        continue;
                    }
                    let existing = spawned
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, e)| *e)
                        .filter(|e| world.get_entity(*e).is_ok());
                    let child = match existing {
                        Some(child) => child,
                        None => {
                            let child = world.spawn(ChildOf(entity)).id();
                            if let Err(err) = world.entity_mut(child).apply_scene(scene) {
                                warn!(
                                    "reactor list on {entity} failed to apply item scene: {err:?}"
                                );
                            }
                            child
                        }
                    };
                    next.push((key, child));
                }
                let ordered: Vec<Entity> = next.iter().map(|(_, e)| *e).collect();
                world.entity_mut(entity).replace_children(&ordered);
                spawned = next;
                new_last = world.change_tick();
            }
            if let Some(mut list) = world.get_mut::<ReactorList>(entity) {
                let list = list.bypass_change_detection();
                list.state = state;
                list.spawned = spawned;
                list.last_run = new_last;
            }
        }

        if !ran_any {
            return;
        }
        #[cfg(debug_assertions)]
        if check_this_frame {
            let watched = watched_cache.get_or_insert_with(|| watched_types(world));
            verify_writes_confined(world, this_run, &written, watched);
        }
        if pass == MAX_PASSES - 1 {
            warn!(
                "reactive BSN did not settle after {MAX_PASSES} passes; \
                 a reactor is probably writing its own dependency"
            );
        }
        written_prev = written;
    }
}

/// All component types watched by entity-targeted deps of live reactors.
/// Only writes to these can break pass-filter soundness, which keeps the
/// debug checker precise: engine bookkeeping written by hooks and observers
/// in response to legitimate writes (`Observer` executions, render-world
/// sync markers, …) is naturally out of scope unless something watches it.
#[cfg(debug_assertions)]
fn watched_types(world: &mut World) -> std::collections::HashSet<std::any::TypeId> {
    let mut watched = std::collections::HashSet::new();
    for reactor in world.query::<&Reactor>().iter(world) {
        for instance in &reactor.instances {
            watched.extend(instance.spec.deps.iter().filter_map(|d| d.watched_type()));
        }
    }
    for list in world.query::<&ReactorList>().iter(world) {
        watched.extend(list.spec.deps.iter().filter_map(|d| d.watched_type()));
    }
    watched
}

/// Debug-build contract check: every *watched* component written during a
/// pass must belong to an entity in the pass's written set or descended
/// from one — i.e. renders only write their own subtree. A violation means
/// a later pass could skip a reactor that should have woken (the pass
/// filter's soundness argument breaks), so it is reported loudly. Note that
/// re-parenting a reactor's entity from inside its scene counts: inserting
/// `ChildOf(other)` mutates `other`'s `Children`.
#[cfg(debug_assertions)]
fn verify_writes_confined(
    world: &World,
    pass_start: Tick,
    written: &EntityHashSet,
    watched: &std::collections::HashSet<std::any::TypeId>,
) {
    let this_run = world.read_change_tick();
    for entity_ref in world.iter_entities() {
        let entity = entity_ref.id();
        if in_written_subtree(world, entity, written) {
            continue;
        }
        for &component_id in entity_ref.archetype().components() {
            let Some(info) = world.components().get_info(component_id) else {
                continue;
            };
            if !info.type_id().is_some_and(|id| watched.contains(&id)) {
                continue;
            }
            let Some(ticks) = entity_ref.get_change_ticks_by_id(component_id) else {
                continue;
            };
            if ticks.is_changed(pass_start, this_run) {
                error!(
                    target: "reactive_bsn",
                    "reactor render wrote `{}` on {entity} — a component type some \
                     reactor watches — outside the rendering reactor's subtree; \
                     convergence-pass filtering may now miss updates. Reactor scenes \
                     (and observers they trigger) must only write the reactor's own \
                     entity and (de)spawned descendants.",
                    info.name()
                );
            }
        }
    }
}

/// Is `entity` in `written`, or descended (via `ChildOf`) from an entity
/// that is? Children spawned by a render are legitimate writes.
#[cfg(debug_assertions)]
fn in_written_subtree(world: &World, mut entity: Entity, written: &EntityHashSet) -> bool {
    loop {
        if written.contains(&entity) {
            return true;
        }
        match world.get::<ChildOf>(entity) {
            Some(child_of) => entity = child_of.parent(),
            None => return false,
        }
    }
}

/// Can this reactor be skipped for a follow-up convergence pass? Yes if it
/// already ran at least once and none of its dependencies could have been
/// affected by the entities the previous pass wrote.
fn skips_pass(
    deps: &[super::Dep],
    last_run: Tick,
    written_prev: &EntityHashSet,
    entity: Entity,
) -> bool {
    last_run != Tick::new(0)
        && !deps
            .iter()
            .any(|dep| dep.maybe_affected(written_prev, entity))
}

/// All descendants of `root` via `Children`, recorded before a despawn so
/// later passes re-check anything that was watching them.
fn collect_descendants(world: &World, root: Entity, out: &mut EntityHashSet) {
    if let Some(children) = world.get::<Children>(root) {
        for child in children.iter() {
            out.insert(child);
            collect_descendants(world, child, out);
        }
    }
}

/// Evaluate *all* deps (so per-dep state stays current) and return the index
/// of the first dirty one, if any.
fn first_dirty_dep(
    world: &mut World,
    scans: &mut SharedScans,
    deps: &[super::Dep],
    state: &mut [DepState],
    this: Entity,
    last_run: Tick,
    this_run: Tick,
) -> Option<usize> {
    let mut cx = DepCx {
        world,
        scans,
        this,
        last_run,
        this_run,
    };
    let mut woke = None;
    for (index, (dep, dep_state)) in deps.iter().zip(state.iter_mut()).enumerate() {
        if dep.check(&mut cx, dep_state) && woke.is_none() {
            woke = Some(index);
        }
    }
    woke
}

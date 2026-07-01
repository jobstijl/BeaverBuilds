//! Reactive dependencies: pure, cloneable *specifications* of "what to watch",
//! answered with Bevy's native change ticks.
//!
//! A [`Dep`] holds no mutable state of its own — per-reactor-instance state
//! (existence flags, member counts) lives in [`DepState`] on the reactor, and
//! whole-world component scans are shared across all reactors through
//! [`SharedScans`], so N reactors watching the same component type pay for
//! one scan per frame instead of N.

use std::any::{TypeId, type_name};
use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::{Asset, AssetEvent, AssetId};
use bevy::ecs::change_detection::{MAX_CHANGE_AGE, Tick};
use bevy::ecs::entity::EntityHashSet;
use bevy::ecs::message::{Message, MessageCursor, Messages};
use bevy::ecs::query::QueryFilter;
use bevy::ecs::relationship::RelationshipTarget;
use bevy::prelude::*;

/// Which entity a dependency points at: a fixed entity, the entity the
/// reactor itself sits on, or that entity's parent (resolved at check time,
/// which is what makes dependencies usable inside `bsn!` templates where no
/// entity exists yet).
#[derive(Clone, Copy)]
enum Target {
    This,
    Parent,
    Fixed(Entity),
}

impl Target {
    fn resolve(self, this: Entity, world: &World) -> Option<Entity> {
        match self {
            Target::This => Some(this),
            Target::Parent => world.get::<ChildOf>(this).map(ChildOf::parent),
            Target::Fixed(e) => Some(e),
        }
    }

    /// `Parent` targets implicitly depend on the `ChildOf` edge itself:
    /// re-parenting changes *which* entity is watched even when neither
    /// parent's component ticked. Detect it via `ChildOf`'s own ticks.
    fn reparented(self, this: Entity, world: &World, last_run: Tick, this_run: Tick) -> bool {
        matches!(self, Target::Parent)
            && world
                .get_entity(this)
                .ok()
                .and_then(|e| e.get_change_ticks::<ChildOf>())
                .is_some_and(|t| t.is_changed(last_run, this_run))
    }

    /// Conservative pass-filter resolution: `Parent` can't be resolved
    /// without the world, so it always re-checks.
    fn maybe_in(self, written: &EntityHashSet, this: Entity) -> bool {
        match self {
            Target::This => written.contains(&this),
            Target::Parent => true,
            Target::Fixed(e) => written.contains(&e),
        }
    }
}

/// Per-reactor-instance mutable state for one dependency.
#[derive(Default)]
pub(crate) struct DepState {
    existed: Option<bool>,
    count: Option<usize>,
    /// Cached projection result for value deps, type-erased.
    cache: Option<Box<dyn std::any::Any + Send + Sync>>,
    /// Tick of the last projection, so quiet sources skip projecting.
    seen: Tick,
}

/// Everything a dependency check may need.
pub(crate) struct DepCx<'a> {
    pub world: &'a mut World,
    pub scans: &'a mut SharedScans,
    pub this: Entity,
    pub last_run: Tick,
    pub this_run: Tick,
}

trait DepSpec: Send + Sync {
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool;
    fn describe(&self) -> String;

    /// Convergence-pass filter: could this dependency have been dirtied by a
    /// pass that wrote exactly the entities in `written`? Reactor renders can
    /// only write components on their target entity and (de)spawned
    /// descendants — they cannot write resources — so an entity-targeted
    /// dependency whose target is outside `written` is provably still clean.
    /// Anything that can't make that argument answers `true` (resources are
    /// O(1) to re-check; whole-world scans are shared per type anyway).
    fn maybe_affected(&self, _written: &EntityHashSet, _this: Entity) -> bool {
        true
    }

    /// The component type this dependency watches on specific entities, if
    /// any. Used by the debug write-contract checker: only writes to watched
    /// types can break pass-filter soundness, so only those are policed.
    fn watched_type(&self) -> Option<std::any::TypeId> {
        None
    }
}

/// A single declared dependency of a reactor. Cheap to clone (`Arc`), free of
/// instance state, so reactor specs can be forked into any number of
/// instances (one per spawned entity).
#[derive(Clone)]
pub struct Dep(Arc<dyn DepSpec>);

impl Dep {
    pub(crate) fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        self.0.check(cx, state)
    }

    pub(crate) fn describe(&self) -> String {
        self.0.describe()
    }

    pub(crate) fn maybe_affected(&self, written: &EntityHashSet, this: Entity) -> bool {
        self.0.maybe_affected(written, this)
    }

    pub(crate) fn watched_type(&self) -> Option<TypeId> {
        self.0.watched_type()
    }
}

fn short_name<T: ?Sized>() -> &'static str {
    type_name::<T>().rsplit("::").next().unwrap_or("?")
}

// ---------------------------------------------------------------------------
// Resource dependency
// ---------------------------------------------------------------------------

struct ResourceDep<R>(std::marker::PhantomData<fn() -> R>);

impl<R: Resource> DepSpec for ResourceDep<R> {
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        let res = cx.world.get_resource_ref::<R>();
        let exists = res.is_some();
        let appeared = state.existed.replace(exists) != Some(exists);
        appeared || res.is_some_and(|r| r.last_changed().is_newer_than(cx.last_run, cx.this_run))
    }

    fn describe(&self) -> String {
        format!("resource {}", short_name::<R>())
    }
}

// ---------------------------------------------------------------------------
// Single-entity component dependency
// ---------------------------------------------------------------------------

struct ComponentDep<T> {
    target: Target,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: Component> DepSpec for ComponentDep<T> {
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        let ticks = self
            .target
            .resolve(cx.this, cx.world)
            .and_then(|entity| cx.world.get_entity(entity).ok())
            .and_then(|e| e.get_change_ticks::<T>());
        let exists = ticks.is_some();
        let appeared = state.existed.replace(exists) != Some(exists);
        appeared
            || self
                .target
                .reparented(cx.this, cx.world, cx.last_run, cx.this_run)
            || ticks.is_some_and(|t| t.is_changed(cx.last_run, cx.this_run))
    }

    fn describe(&self) -> String {
        format!("component {}", short_name::<T>())
    }

    fn maybe_affected(&self, written: &EntityHashSet, this: Entity) -> bool {
        self.target.maybe_in(written, this)
    }

    fn watched_type(&self) -> Option<TypeId> {
        Some(TypeId::of::<T>())
    }
}

// ---------------------------------------------------------------------------
// Component presence dependency (insert/remove only)
// ---------------------------------------------------------------------------

struct PresenceDep<T> {
    target: Target,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: Component> DepSpec for PresenceDep<T> {
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        let ticks = self
            .target
            .resolve(cx.this, cx.world)
            .and_then(|entity| cx.world.get_entity(entity).ok())
            .and_then(|e| e.get_change_ticks::<T>());
        let exists = ticks.is_some();
        let appeared = state.existed.replace(exists) != Some(exists)
            || self
                .target
                .reparented(cx.this, cx.world, cx.last_run, cx.this_run);
        // The `added` tick (not `changed`) also catches remove-then-reinsert
        // between checks; plain mutations are deliberately ignored.
        appeared || ticks.is_some_and(|t| t.is_added(cx.last_run, cx.this_run))
    }

    fn describe(&self) -> String {
        format!("presence of {}", short_name::<T>())
    }

    fn maybe_affected(&self, written: &EntityHashSet, this: Entity) -> bool {
        self.target.maybe_in(written, this)
    }

    fn watched_type(&self) -> Option<TypeId> {
        Some(TypeId::of::<T>())
    }
}

// ---------------------------------------------------------------------------
// Whole-world component dependency, amortized via SharedScans
// ---------------------------------------------------------------------------

struct ComponentsDep<T, F>(std::marker::PhantomData<fn() -> (T, F)>);

impl<T: Component, F: QueryFilter + 'static> DepSpec for ComponentsDep<T, F> {
    fn check(&self, cx: &mut DepCx, _state: &mut DepState) -> bool {
        let stamp = cx.scans.stamp::<T, F>(cx.world, cx.this_run);
        stamp.is_newer_than(cx.last_run, cx.this_run)
    }

    fn describe(&self) -> String {
        format!("components {}", short_name::<T>())
    }
}

// ---------------------------------------------------------------------------
// Relationship dependencies
// ---------------------------------------------------------------------------

/// Reacts when component `T` changes on *any entity related to the target*
/// via the relationship collection `S` (e.g. `Children`), or when the
/// relation set itself changes.
struct RelatedComponentsDep<S, T> {
    target: Target,
    _marker: std::marker::PhantomData<fn() -> (S, T)>,
}

impl<S: RelationshipTarget, T: Component> DepSpec for RelatedComponentsDep<S, T> {
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        let Some(entity) = self.target.resolve(cx.this, cx.world) else {
            return false;
        };
        let (mut dirty, members) = match cx.world.get_entity(entity) {
            Ok(entity_ref) => {
                let set_changed = entity_ref
                    .get_change_ticks::<S>()
                    .is_some_and(|t| t.is_changed(cx.last_run, cx.this_run));
                let members: Vec<Entity> = entity_ref
                    .get::<S>()
                    .map(|s| s.iter().collect())
                    .unwrap_or_default();
                (set_changed, members)
            }
            Err(_) => (false, Vec::new()),
        };
        // Count members that carry T, so a member losing T (or despawning)
        // is detected even though its ticks are gone.
        let mut count = 0;
        for member in members {
            if let Some(ticks) = cx
                .world
                .get_entity(member)
                .ok()
                .and_then(|m| m.get_change_ticks::<T>())
            {
                count += 1;
                if ticks.is_changed(cx.last_run, cx.this_run) {
                    dirty = true;
                }
            }
        }
        if state.count.replace(count) != Some(count) {
            dirty = true;
        }
        dirty
    }

    fn describe(&self) -> String {
        format!(
            "{} on entities related via {}",
            short_name::<T>(),
            short_name::<S>()
        )
    }

    fn watched_type(&self) -> Option<TypeId> {
        // The member component; the relation set `S` itself is covered by
        // this dep always re-checking (`maybe_affected` is `true`).
        Some(TypeId::of::<T>())
    }
}

// ---------------------------------------------------------------------------
// Value-projection dependencies: per-field wake granularity, today
// ---------------------------------------------------------------------------

/// Wakes only when a *projection* of a resource changes value. The
/// projection is tick-gated: it runs only when the resource's component
/// tick advanced, so quiet resources cost one tick compare and noisy
/// resources with stable projections cost one projection + `PartialEq`.
struct ResourceValueDep<R, V, F> {
    project: F,
    _marker: std::marker::PhantomData<fn() -> (R, V)>,
}

impl<R, V, F> DepSpec for ResourceValueDep<R, V, F>
where
    R: Resource,
    V: PartialEq + Send + Sync + 'static,
    F: Fn(&R) -> V + Send + Sync,
{
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        clamp_tick(&mut state.seen, cx.this_run);
        let Some(res) = cx.world.get_resource_ref::<R>() else {
            // Resource vanished: dirty once, then quiet.
            return state.cache.take().is_some();
        };
        if state.cache.is_some() && !res.last_changed().is_newer_than(state.seen, cx.this_run) {
            return false;
        }
        state.seen = cx.this_run;
        let value = (self.project)(&res);
        match state.cache.as_ref().and_then(|c| c.downcast_ref::<V>()) {
            Some(old) if *old == value => false,
            _ => {
                state.cache = Some(Box::new(value));
                true
            }
        }
    }

    fn describe(&self) -> String {
        format!("projected value of resource {}", short_name::<R>())
    }
}

/// Like [`ResourceValueDep`], over component `T` on a *targeted* entity — the
/// reactor's own entity (`this_value`), a fixed entity (`entity_value`), or the
/// `ChildOf` parent (`parent_value`).
struct ComponentValueDep<T, V, F> {
    target: Target,
    project: F,
    _marker: std::marker::PhantomData<fn() -> (T, V)>,
}

impl<T, V, F> DepSpec for ComponentValueDep<T, V, F>
where
    T: Component,
    V: PartialEq + Send + Sync + 'static,
    F: Fn(&T) -> V + Send + Sync,
{
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        clamp_tick(&mut state.seen, cx.this_run);
        let entity = self
            .target
            .resolve(cx.this, cx.world)
            .and_then(|e| cx.world.get_entity(e).ok());
        let ticks = entity.as_ref().and_then(|e| e.get_change_ticks::<T>());
        // A `Parent` target whose `ChildOf` edge moved now points at a
        // *different* entity, so the projected value may differ even though the
        // new parent's `T` tick is old — bypass the tick-gate in that case and
        // let the `PartialEq` compare decide.
        let reparented = self
            .target
            .reparented(cx.this, cx.world, state.seen, cx.this_run);
        let (Some(entity), Some(ticks)) = (entity, ticks) else {
            return state.cache.take().is_some();
        };
        if state.cache.is_some() && !reparented && !ticks.is_changed(state.seen, cx.this_run) {
            return false;
        }
        state.seen = cx.this_run;
        let value = (self.project)(entity.get::<T>().unwrap());
        match state.cache.as_ref().and_then(|c| c.downcast_ref::<V>()) {
            Some(old) if *old == value => false,
            _ => {
                state.cache = Some(Box::new(value));
                true
            }
        }
    }

    fn describe(&self) -> String {
        format!("projected value of component {}", short_name::<T>())
    }

    fn maybe_affected(&self, written: &EntityHashSet, this: Entity) -> bool {
        self.target.maybe_in(written, this)
    }

    fn watched_type(&self) -> Option<TypeId> {
        Some(TypeId::of::<T>())
    }
}

// ---------------------------------------------------------------------------
// Ancestor dependencies: the ECS analog of React Context
// ---------------------------------------------------------------------------

/// Defensive cap on the `ChildOf` walk: a well-formed hierarchy never cycles,
/// but a malformed one shouldn't loop forever.
const MAX_ANCESTOR_DEPTH: usize = 256;

/// Walk `ChildOf` upward from `entity` to the nearest *strict ancestor*
/// carrying `T`. Shared by [`Dep::ancestor`] and [`WorldAncestorExt`] so the
/// dependency and the value read always agree on which entity is "nearest".
fn nearest_ancestor_entity<T: Component>(world: &World, entity: Entity) -> Option<Entity> {
    let mut cur = world.get::<ChildOf>(entity)?.parent();
    for _ in 0..MAX_ANCESTOR_DEPTH {
        if world.get::<T>(cur).is_some() {
            return Some(cur);
        }
        cur = world.get::<ChildOf>(cur)?.parent();
    }
    bevy::log::warn_once!(
        "ancestor walk for {} from {} gave up after {} levels — is the ChildOf hierarchy cyclic?",
        short_name::<T>(),
        entity,
        MAX_ANCESTOR_DEPTH
    );
    None
}

/// Reacts to component `T` on the nearest strict `ChildOf` ancestor carrying
/// it. Wakes on the provider's value change, on re-parenting that changes which
/// ancestor is nearest, and on the provider appearing/disappearing.
struct AncestorDep<T> {
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: Component> DepSpec for AncestorDep<T> {
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        let provider = nearest_ancestor_entity::<T>(cx.world, cx.this);
        // Provider identity (appeared / vanished / swapped via re-parenting
        // anywhere up the chain) is tracked in the type-erased cache slot; on
        // the first check `prev` is `None`, so a new watcher fires once.
        let prev = state
            .cache
            .as_ref()
            .and_then(|c| c.downcast_ref::<Option<Entity>>())
            .copied();
        let identity_changed = prev != Some(provider);
        state.cache = Some(Box::new(provider));
        let value_changed = provider
            .and_then(|e| cx.world.get_entity(e).ok())
            .and_then(|e| e.get_change_ticks::<T>())
            .is_some_and(|t| t.is_changed(cx.last_run, cx.this_run));
        identity_changed || value_changed
    }

    fn describe(&self) -> String {
        format!("nearest ancestor with {}", short_name::<T>())
    }

    fn watched_type(&self) -> Option<TypeId> {
        Some(TypeId::of::<T>())
    }
}

/// [`AncestorDep`] with a value projection. The ancestor *walk* re-runs each
/// check — provider identity has no tick to gate on — but the projection is
/// tick-gated against the resolved provider (cached alongside the value): a
/// quiet same-provider check costs the walk plus one tick compare, and the
/// projection + `PartialEq` run only when the provider or its `T` changed.
struct AncestorValueDep<T, V, F> {
    project: F,
    _marker: std::marker::PhantomData<fn() -> (T, V)>,
}

impl<T, V, F> DepSpec for AncestorValueDep<T, V, F>
where
    T: Component,
    V: PartialEq + Send + Sync + 'static,
    F: Fn(&T) -> V + Send + Sync,
{
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        clamp_tick(&mut state.seen, cx.this_run);
        let Some(provider) = nearest_ancestor_entity::<T>(cx.world, cx.this) else {
            // Provider vanished: dirty once, then quiet.
            return state.cache.take().is_some();
        };
        let same_provider = state
            .cache
            .as_ref()
            .and_then(|c| c.downcast_ref::<(Entity, V)>())
            .is_some_and(|(prev, _)| *prev == provider);
        let ticks = cx
            .world
            .get_entity(provider)
            .ok()
            .and_then(|e| e.get_change_ticks::<T>());
        // A swapped provider (re-parent, nearer/removed provider) may hold a
        // different value under an old tick, so only an *unchanged* provider
        // gets the tick gate.
        if same_provider && ticks.is_some_and(|t| !t.is_changed(state.seen, cx.this_run)) {
            return false;
        }
        state.seen = cx.this_run;
        let value = (self.project)(cx.world.get::<T>(provider).unwrap());
        let changed = match state
            .cache
            .as_ref()
            .and_then(|c| c.downcast_ref::<(Entity, V)>())
        {
            Some((_, old)) => *old != value,
            None => true,
        };
        state.cache = Some(Box::new((provider, value)));
        changed
    }

    fn describe(&self) -> String {
        format!(
            "projected value of nearest ancestor with {}",
            short_name::<T>()
        )
    }

    fn watched_type(&self) -> Option<TypeId> {
        Some(TypeId::of::<T>())
    }
}

/// Read access matching [`Dep::ancestor`]: the nearest strict `ChildOf`
/// ancestor of `entity` carrying `T`. Use it inside a render closure so the
/// read walks the same path the dependency wakes on.
pub trait WorldAncestorExt {
    /// The nearest strict ancestor of `entity` carrying `T`, if any.
    fn nearest_ancestor<T: Component>(&self, entity: Entity) -> Option<&T>;
}

impl WorldAncestorExt for World {
    fn nearest_ancestor<T: Component>(&self, entity: Entity) -> Option<&T> {
        nearest_ancestor_entity::<T>(self, entity).and_then(|e| self.get::<T>(e))
    }
}

// ---------------------------------------------------------------------------
// Resource presence dependency (insert/remove only)
// ---------------------------------------------------------------------------

struct ResourcePresenceDep<R>(std::marker::PhantomData<fn() -> R>);

impl<R: Resource> DepSpec for ResourcePresenceDep<R> {
    fn check(&self, cx: &mut DepCx, state: &mut DepState) -> bool {
        // Existence-only: wakes on insert/remove across checks, ignores
        // mutations. A remove-then-reinsert within one check window nets to no
        // change and is not distinguished (rare for resources, unlike
        // component presence which has an `added`-tick to lean on).
        let exists = cx.world.contains_resource::<R>();
        state.existed.replace(exists) != Some(exists)
    }

    fn describe(&self) -> String {
        format!("presence of resource {}", short_name::<R>())
    }
}

// ---------------------------------------------------------------------------
// Message-buffer dependencies (plain messages and per-asset events)
//
// Message buffers have no change ticks, but their write heads are monotone:
// a shared per-type stamp (the buffer analog of `SharedScans`) reads each
// buffer once per runner tick and records the tick at which new messages
// last arrived. Deps compare that stamp against their fragment's `last_run`
// exactly like scan stamps — idempotent across convergence passes, so no
// per-dep cursor consumption can double-fire or under-fire.
// ---------------------------------------------------------------------------

/// Reacts when one or more `E` messages were written since the last check.
/// Edge-triggered: one wake per burst, however many messages arrived.
struct MessageDep<E>(std::marker::PhantomData<fn() -> E>);

impl<E: Message> DepSpec for MessageDep<E> {
    fn check(&self, cx: &mut DepCx, _state: &mut DepState) -> bool {
        let stamp = cx.scans.message_stamp::<E>(cx.world, cx.this_run);
        stamp.is_newer_than(cx.last_run, cx.this_run)
    }

    fn describe(&self) -> String {
        format!("messages of {}", short_name::<E>())
    }
}

/// Reacts to [`AssetEvent`]s concerning one specific asset id.
struct AssetDep<A: Asset> {
    id: AssetId<A>,
}

impl<A: Asset> DepSpec for AssetDep<A> {
    fn check(&self, cx: &mut DepCx, _state: &mut DepState) -> bool {
        let stamp = cx.scans.asset_stamp::<A>(cx.world, cx.this_run, self.id);
        stamp.is_newer_than(cx.last_run, cx.this_run)
    }

    fn describe(&self) -> String {
        format!("asset {} ({:?})", short_name::<A>(), self.id)
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl Dep {
    /// React to changes of resource `R` (including insertion/removal).
    ///
    /// Note: `Dep::resource::<Assets<T>>()` reacts to any asset mutation in
    /// that collection (e.g. hot reloads).
    pub fn resource<R: Resource>() -> Dep {
        Dep(Arc::new(ResourceDep::<R>(std::marker::PhantomData)))
    }

    /// React only when a *projection* of resource `R` changes value — field
    /// (or derived-value) wake granularity, expressible today: the closure
    /// runs only when `R`'s tick advanced (tick-gated), and the reactor
    /// wakes only when the projected value differs from the cached one.
    ///
    /// ```ignore
    /// // Re-render once per displayed second, not 60×/sec:
    /// Dep::resource_value(|s: &Season| s.remaining.ceil() as u32)
    /// ```
    pub fn resource_value<R, V>(project: impl Fn(&R) -> V + Send + Sync + 'static) -> Dep
    where
        R: Resource,
        V: PartialEq + Send + Sync + 'static,
    {
        Dep(Arc::new(ResourceValueDep::<R, V, _> {
            project,
            _marker: std::marker::PhantomData,
        }))
    }

    /// [`Dep::resource_value`], over component `T` on the reactor's own
    /// entity.
    pub fn this_value<T, V>(project: impl Fn(&T) -> V + Send + Sync + 'static) -> Dep
    where
        T: Component,
        V: PartialEq + Send + Sync + 'static,
    {
        Dep(Arc::new(ComponentValueDep::<T, V, _> {
            target: Target::This,
            project,
            _marker: std::marker::PhantomData,
        }))
    }

    /// [`Dep::resource_value`], over component `T` on one specific entity — the
    /// per-field companion to [`Dep::entity`]. Lets a widget watching state on
    /// a passed-in entity wake on just one field of a bundled state component
    /// instead of on every field (one fat `WidgetState` component, several
    /// independent projection fragments).
    pub fn entity_value<T, V>(
        entity: Entity,
        project: impl Fn(&T) -> V + Send + Sync + 'static,
    ) -> Dep
    where
        T: Component,
        V: PartialEq + Send + Sync + 'static,
    {
        Dep(Arc::new(ComponentValueDep::<T, V, _> {
            target: Target::Fixed(entity),
            project,
            _marker: std::marker::PhantomData,
        }))
    }

    /// [`Dep::resource_value`], over component `T` on the reactor entity's
    /// `ChildOf` parent — re-parenting re-projects against the new parent.
    pub fn parent_value<T, V>(project: impl Fn(&T) -> V + Send + Sync + 'static) -> Dep
    where
        T: Component,
        V: PartialEq + Send + Sync + 'static,
    {
        Dep(Arc::new(ComponentValueDep::<T, V, _> {
            target: Target::Parent,
            project,
            _marker: std::marker::PhantomData,
        }))
    }

    /// React to changes of component `T` on the reactor's *own* entity,
    /// including insert/remove. This is the dependency to use inside
    /// [`reactive`](crate::reactive) templates.
    pub fn this<T: Component>() -> Dep {
        Dep(Arc::new(ComponentDep::<T> {
            target: Target::This,
            _marker: std::marker::PhantomData,
        }))
    }

    /// React to changes of component `T` on one specific entity, including
    /// insert/remove (and the entity itself disappearing).
    pub fn entity<T: Component>(entity: Entity) -> Dep {
        Dep(Arc::new(ComponentDep::<T> {
            target: Target::Fixed(entity),
            _marker: std::marker::PhantomData,
        }))
    }

    /// React to changes of component `T` on the reactor's entity's *parent*
    /// (via `ChildOf`), including insert/remove. Useful for child fragments
    /// rendering state their parent owns (e.g. [`reactive_async`] results).
    ///
    /// [`reactive_async`]: crate::reactive_async
    pub fn parent<T: Component>() -> Dep {
        Dep(Arc::new(ComponentDep::<T> {
            target: Target::Parent,
            _marker: std::marker::PhantomData,
        }))
    }

    /// React to component `T` on the nearest *strict ancestor* (walking
    /// `ChildOf`) that carries it — the ECS analog of React Context: "provide"
    /// by inserting `T` on a container entity, "inject" with this dep.
    /// Generalizes [`Dep::parent`] (the distance-1 case) and decouples a widget
    /// from *which* ancestor holds the state. Wakes when that component
    /// changes, when re-parenting changes which ancestor is nearest, and when
    /// the provider appears or disappears.
    ///
    /// Read the value in the render closure with
    /// [`WorldAncestorExt::nearest_ancestor`], which walks the same path.
    pub fn ancestor<T: Component>() -> Dep {
        Dep(Arc::new(AncestorDep::<T> {
            _marker: std::marker::PhantomData,
        }))
    }

    /// [`Dep::ancestor`] with a value projection: wakes only when a *projection*
    /// of the nearest ancestor's `T` changes value. The ancestor walk re-runs
    /// each check (provider identity can change with no tick advancing;
    /// ancestors are shallow, so this stays cheap), but the projection itself
    /// is tick-gated against the resolved provider, like
    /// [`Dep::resource_value`]/[`Dep::this_value`].
    pub fn ancestor_value<T, V>(project: impl Fn(&T) -> V + Send + Sync + 'static) -> Dep
    where
        T: Component,
        V: PartialEq + Send + Sync + 'static,
    {
        Dep(Arc::new(AncestorValueDep::<T, V, _> {
            project,
            _marker: std::marker::PhantomData,
        }))
    }

    /// React only when component `T` is inserted on / removed from `entity`,
    /// ignoring mutations. Use for "mode switch" components whose fields also
    /// tick every frame (e.g. a construction-progress component), typically
    /// pairing a structural `rebuild` on presence with a `patch` on value.
    pub fn presence<T: Component>(entity: Entity) -> Dep {
        Dep(Arc::new(PresenceDep::<T> {
            target: Target::Fixed(entity),
            _marker: std::marker::PhantomData,
        }))
    }

    /// [`Dep::presence`] targeting the reactor's own entity.
    pub fn presence_this<T: Component>() -> Dep {
        Dep(Arc::new(PresenceDep::<T> {
            target: Target::This,
            _marker: std::marker::PhantomData,
        }))
    }

    /// React only when resource `R` is inserted or removed, ignoring mutations
    /// — the resource analog of [`Dep::presence`]. (A remove-then-reinsert
    /// within one check window nets to no change and is not distinguished.)
    pub fn resource_presence<R: Resource>() -> Dep {
        Dep(Arc::new(ResourcePresenceDep::<R>(std::marker::PhantomData)))
    }

    /// React when one or more `E` messages were written since the last check.
    /// Edge-triggered: one wake per burst however many messages arrived, and
    /// nothing carries the messages into the render — read *state* there, not
    /// the buffer. When the message isn't itself the whole signal, prefer a
    /// plain system that consumes it and writes state reactors watch; this
    /// dep is for messages that *are* the state change (a "settings changed"
    /// ping, a save completed). Messages written after the reactor runner in
    /// the schedule wake it next frame. The underlying buffer read is shared:
    /// any number of watchers of `E` cost one read per frame, total.
    pub fn message<E: Message>() -> Dep {
        Dep(Arc::new(MessageDep::<E>(std::marker::PhantomData)))
    }

    /// React to [`AssetEvent`]s concerning one specific asset — added,
    /// modified (hot reloads included), removed, unused — the per-handle
    /// refinement of `Dep::resource::<Assets<A>>()`, which wakes on *any*
    /// asset of the type. Takes an [`AssetId`] (a `&Handle<A>` converts), so
    /// the dependency never keeps the asset alive. The `AssetEvent<A>` sweep
    /// is shared: any number of per-asset watchers cost one buffer read per
    /// frame, total.
    pub fn asset<A: Asset>(id: impl Into<AssetId<A>>) -> Dep {
        Dep(Arc::new(AssetDep::<A> { id: id.into() }))
    }

    /// React when *any* entity's `T` changes, or entities with `T` are added
    /// or removed. The underlying scan is shared: any number of reactors with
    /// this dependency cost one scan of `T` per frame, total.
    pub fn components<T: Component>() -> Dep {
        Self::components_filtered::<T, ()>()
    }

    /// Like [`Dep::components`], restricted by an extra query filter.
    pub fn components_filtered<T: Component, F: QueryFilter + 'static>() -> Dep {
        Dep(Arc::new(ComponentsDep::<T, F>(std::marker::PhantomData)))
    }

    /// React when `entity`'s relation set `S` (a `RelationshipTarget` like
    /// `Children`) gains, loses or reorders members.
    pub fn related<S: RelationshipTarget>(entity: Entity) -> Dep {
        Self::entity::<S>(entity)
    }

    /// [`Dep::related`] targeting the reactor's own entity.
    pub fn related_this<S: RelationshipTarget>() -> Dep {
        Self::this::<S>()
    }

    /// React when component `T` changes on any entity related to `entity`
    /// via `S` — e.g. "any of this lodge's `Children`'s `Beaver` changed" —
    /// or when the relation set itself changes.
    pub fn related_components<S: RelationshipTarget, T: Component>(entity: Entity) -> Dep {
        Dep(Arc::new(RelatedComponentsDep::<S, T> {
            target: Target::Fixed(entity),
            _marker: std::marker::PhantomData,
        }))
    }

    /// [`Dep::related_components`] targeting the reactor's own entity.
    pub fn related_components_this<S: RelationshipTarget, T: Component>() -> Dep {
        Dep(Arc::new(RelatedComponentsDep::<S, T> {
            target: Target::This,
            _marker: std::marker::PhantomData,
        }))
    }
}

// ---------------------------------------------------------------------------
// Shared scans
// ---------------------------------------------------------------------------

/// One persistent scan per watched component type (+ filter), shared by all
/// reactors. Each runner pass, the first reactor to ask about a type triggers
/// one walk of that type's entities; everyone else reuses the resulting
/// dirty stamp. Message-buffer stamps (plain messages, per-asset events)
/// live here too, under the same once-per-tick refresh discipline.
#[derive(Default)]
pub(crate) struct SharedScans {
    map: HashMap<TypeId, Box<dyn AnyScan + Send + Sync>>,
    /// Keyed by the concrete stamp type (`MessageStamp<E>` / `AssetStamp<A>`),
    /// so a plain `Dep::message::<AssetEvent<A>>()` watcher can coexist with
    /// `Dep::asset::<A>` watchers without colliding.
    messages: HashMap<TypeId, Box<dyn std::any::Any + Send + Sync>>,
}

impl SharedScans {
    fn stamp<T: Component, F: QueryFilter + 'static>(
        &mut self,
        world: &mut World,
        this_run: Tick,
    ) -> Tick {
        let scan = self
            .map
            .entry(TypeId::of::<(T, F)>())
            .or_insert_with(|| Box::new(TypeScan::<T, F>::new(this_run)));
        scan.refresh(world, this_run);
        scan.dirty_stamp()
    }

    fn message_stamp<E: Message>(&mut self, world: &World, this_run: Tick) -> Tick {
        let stamp: &mut MessageStamp<E> = self
            .messages
            .entry(TypeId::of::<MessageStamp<E>>())
            .or_insert_with(|| Box::new(MessageStamp::<E>::new(this_run)))
            .downcast_mut()
            .expect("message stamps are keyed by their concrete type");
        stamp.refresh(world, this_run);
        stamp.dirty_stamp
    }

    fn asset_stamp<A: Asset>(&mut self, world: &World, this_run: Tick, id: AssetId<A>) -> Tick {
        let stamp: &mut AssetStamp<A> = self
            .messages
            .entry(TypeId::of::<AssetStamp<A>>())
            .or_insert_with(|| Box::new(AssetStamp::<A>::new(this_run)))
            .downcast_mut()
            .expect("asset stamps are keyed by their concrete type");
        stamp.refresh(world, this_run);
        stamp.stamp(id)
    }
}

/// Shared per-message-type stamp: the buffer's monotone write head, read once
/// per runner tick and translated into a synthetic change tick.
struct MessageStamp<E: Message> {
    cursor: MessageCursor<E>,
    scanned_at: Option<Tick>,
    dirty_stamp: Tick,
}

impl<E: Message> MessageStamp<E> {
    fn new(this_run: Tick) -> Self {
        Self {
            cursor: MessageCursor::default(),
            scanned_at: None,
            // New watchers (last_run = 0) should fire once on creation; any
            // backlog buffered before the first refresh folds into this same
            // stamp rather than re-firing later.
            dirty_stamp: this_run,
        }
    }

    fn refresh(&mut self, world: &World, this_run: Tick) {
        if self.scanned_at == Some(this_run) {
            return;
        }
        self.scanned_at = Some(this_run);
        clamp_tick(&mut self.dirty_stamp, this_run);
        let Some(messages) = world.get_resource::<Messages<E>>() else {
            return;
        };
        if self.cursor.len(messages) > 0 {
            self.cursor.clear(messages);
            self.dirty_stamp = this_run;
        }
    }
}

/// [`MessageStamp`] over `AssetEvent<A>`, resolved per asset id: one shared
/// buffer read per runner tick records which assets were touched and when.
/// The map holds one tick per touched asset — bounded by the collection —
/// and entries past `MAX_CHANGE_AGE` are pruned (an expired stamp and a
/// pruned one read the same: "not newer than any live `last_run`").
struct AssetStamp<A: Asset> {
    cursor: MessageCursor<AssetEvent<A>>,
    scanned_at: Option<Tick>,
    /// Fire-once stamp for ids no event has touched yet.
    created: Tick,
    touched: HashMap<AssetId<A>, Tick>,
}

impl<A: Asset> AssetStamp<A> {
    fn new(this_run: Tick) -> Self {
        Self {
            cursor: MessageCursor::default(),
            scanned_at: None,
            created: this_run,
            touched: HashMap::new(),
        }
    }

    fn refresh(&mut self, world: &World, this_run: Tick) {
        if self.scanned_at == Some(this_run) {
            return;
        }
        self.scanned_at = Some(this_run);
        clamp_tick(&mut self.created, this_run);
        self.touched
            .retain(|_, tick| this_run.get().wrapping_sub(tick.get()) <= MAX_CHANGE_AGE);
        let Some(messages) = world.get_resource::<Messages<AssetEvent<A>>>() else {
            return;
        };
        for event in self.cursor.read(messages) {
            let (AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::Removed { id }
            | AssetEvent::Unused { id }
            | AssetEvent::LoadedWithDependencies { id }) = event;
            self.touched.insert(*id, this_run);
        }
    }

    fn stamp(&self, id: AssetId<A>) -> Tick {
        self.touched.get(&id).copied().unwrap_or(self.created)
    }
}

trait AnyScan {
    fn refresh(&mut self, world: &mut World, this_run: Tick);
    fn dirty_stamp(&self) -> Tick;
}

struct TypeScan<T: Component, F: QueryFilter> {
    state: Option<QueryState<Ref<'static, T>, F>>,
    last_scan: Tick,
    scanned_at: Option<Tick>,
    dirty_stamp: Tick,
    last_count: usize,
}

impl<T: Component, F: QueryFilter> TypeScan<T, F> {
    fn new(this_run: Tick) -> Self {
        Self {
            state: None,
            last_scan: Tick::new(0),
            scanned_at: None,
            // New watchers (last_run = 0) should fire once on creation.
            dirty_stamp: this_run,
            last_count: usize::MAX,
        }
    }
}

impl<T: Component, F: QueryFilter + 'static> AnyScan for TypeScan<T, F> {
    fn refresh(&mut self, world: &mut World, this_run: Tick) {
        if self.scanned_at == Some(this_run) {
            return;
        }
        self.scanned_at = Some(this_run);
        clamp_tick(&mut self.last_scan, this_run);
        clamp_tick(&mut self.dirty_stamp, this_run);
        let state = self.state.get_or_insert_with(|| QueryState::new(world));
        let mut count = 0;
        let mut dirty = false;
        for item in state.iter(world) {
            count += 1;
            if item.last_changed().is_newer_than(self.last_scan, this_run) {
                dirty = true;
            }
        }
        if count != self.last_count {
            dirty = true;
            self.last_count = count;
        }
        self.last_scan = this_run;
        if dirty {
            self.dirty_stamp = this_run;
        }
    }

    fn dirty_stamp(&self) -> Tick {
        self.dirty_stamp
    }
}

/// Keep a stored tick within `MAX_CHANGE_AGE` of the present so tick
/// wraparound on multi-day sessions can't make old ticks read as new.
/// (Bevy clamps its own stored ticks the same way via `check_change_ticks`.)
pub(crate) fn clamp_tick(tick: &mut Tick, this_run: Tick) {
    if this_run.get().wrapping_sub(tick.get()) > MAX_CHANGE_AGE {
        *tick = Tick::new(this_run.get().wrapping_sub(MAX_CHANGE_AGE));
    }
}

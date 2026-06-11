//! Reactive BSN: a reactive layer on top of Bevy 0.19's BSN scene system,
//! following the direction Cart sketched in bevy#14437 ("fine grained
//! observer-style reactivity") and the lessons of bevy_reactor, jonmo and
//! bevy_cobweb:
//!
//! - **No shadow runtime.** There are no signal cells. Reactive state *is*
//!   ordinary ECS state; dirtiness comes straight from Bevy's native change
//!   ticks. Anything any system writes — with no special wrapper — can drive
//!   a reactor.
//! - **Declared dependencies** ([`Dep::resource`], [`Dep::this`],
//!   [`Dep::entity`], [`Dep::components`], [`Dep::related_components`]) —
//!   debuggable, no hook-ordering footguns, 1:1 onto change detection.
//!   Whole-world component dependencies are checked through one *shared scan
//!   per type per frame*, however many reactors watch the type.
//! - **Incremental updates via BSN patches.** A dirty reactor re-runs its
//!   scene function and re-applies the result onto its own entity; component
//!   patches merge in place, so focus/hover/animation state survives.
//! - **Ownership via the entity graph.** A reactor is a component; despawning
//!   the entity tears everything down.
//! - **Composable inside `bsn!`.** [`reactive`], [`reactive_rebuild`] and
//!   [`reactive_list`] return `impl Scene`, so reactive fragments are
//!   declared inline in scene trees (each spawn forks a fresh reactor
//!   instance):
//!
//! ```ignore
//! #[derive(Resource, Default)]
//! struct Score(u32);
//!
//! bsn! {
//!     Node Children [
//!         reactive([Dep::resource::<Score>()], |world: &World, _: Entity| {
//!             let score = world.resource::<Score>().0;
//!             bsn! { Text({ format!("Score: {score}") }) }
//!         }),
//!     ]
//! }
//! ```
//!
//! No special syntax is needed: BSN already parses these as ordinary
//! scene-function includes, exactly like composing `button("Ok")`.
//!
//! Dynamic children remain explicit, because BSN's `apply_scene` re-spawns
//! `Children [..]` on every application: [`Reactor::patch`]/[`reactive`] for
//! childless in-place fragments, [`Reactor::rebuild`]/[`reactive_rebuild`]
//! to replace a subtree, and [`ReactorList`]/[`reactive_list`] for keyed
//! collections — the list reconciles *membership and order only*; item
//! content updates by embedding `reactive(...)` fragments in the items.
//!
//! Reactors run in an exclusive system in `Update` (set [`ReactSet`]),
//! looping until no reactor is dirty so chains settle within a frame
//! (capped, with a divergence warning). Follow-up passes are cheap: renders
//! can only write their own target entity and (de)spawned descendants —
//! never resources — so passes after the first skip every reactor whose
//! entity-targeted deps point outside the previous pass's written set.
//! Wake-ups are traced at `debug` level under the `reactive_bsn` target
//! (`RUST_LOG=reactive_bsn=debug`). One `Reactor` and one `ReactorList` per
//! entity.
//!
//! **The write contract:** applying a reactor's scene may only write
//! components on the reactor's own entity and its (de)spawned descendants.
//! The pass filter's soundness depends on this — resource writes are
//! harmless (resource deps are re-checked every pass), but writing another
//! entity's components (including indirectly, e.g. inserting
//! `ChildOf(other)`, which mutates `other`'s `Children`) could make a later
//! pass skip a reactor that should have woken. The failure mode is bounded:
//! every frame's first pass checks all reactors unconditionally, so a
//! violation degrades to a one-frame delay for the affected watcher, never
//! a lost update. Debug builds verify the contract after every active pass
//! (scoped to component types some reactor watches) and log violations
//! under the `reactive_bsn` target.

mod async_resource;
mod dep;
mod runner;

use std::sync::Arc;

use bevy::ecs::change_detection::Tick;
use bevy::ecs::template::template;
use bevy::prelude::*;
use bevy::scene::SpawnSceneError;

pub use async_resource::{AsyncValue, reactive_async};
pub use dep::Dep;
use dep::DepState;

pub struct ReactiveBsnPlugin;

impl Plugin for ReactiveBsnPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (async_resource::drive_async_slots, runner::run_reactors)
                .chain()
                .in_set(ReactSet),
        );
    }
}

/// Systems in `Update` that should run before reactors react can order
/// themselves `.before(ReactSet)`.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ReactSet;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    Patch,
    Rebuild,
}

type RenderFn = dyn Fn(&mut World, Entity) -> Result<(), SpawnSceneError> + Send + Sync;

/// The shared, immutable part of a reactor: dependencies + scene function +
/// mode. Cheap to clone; fork instances with [`Reactor::from_spec`]. Build
/// one spec outside a loop when attaching the same reactor to many entities
/// (e.g. one per map tile).
#[derive(Clone)]
pub struct ReactorSpec {
    pub(crate) deps: Arc<[Dep]>,
    pub(crate) render: Arc<RenderFn>,
    pub(crate) mode: Mode,
}

impl ReactorSpec {
    /// In-place reactive fragment: the scene's component patches are merged
    /// onto the entity on every run. The scene must not contain `Children`.
    pub fn patch<S, F>(deps: impl IntoIterator<Item = Dep>, scene_fn: F) -> Self
    where
        S: bevy::scene::Scene,
        F: Fn(&World, Entity) -> S + Send + Sync + 'static,
    {
        Self::new(deps, scene_fn, Mode::Patch)
    }

    /// Structural reactive fragment: on every run the entity's children are
    /// despawned and the scene (children included) is applied fresh.
    pub fn rebuild<S, F>(deps: impl IntoIterator<Item = Dep>, scene_fn: F) -> Self
    where
        S: bevy::scene::Scene,
        F: Fn(&World, Entity) -> S + Send + Sync + 'static,
    {
        Self::new(deps, scene_fn, Mode::Rebuild)
    }

    fn new<S, F>(deps: impl IntoIterator<Item = Dep>, scene_fn: F, mode: Mode) -> Self
    where
        S: bevy::scene::Scene,
        F: Fn(&World, Entity) -> S + Send + Sync + 'static,
    {
        // Type-erase at the closure boundary: `bsn!` types are unnameable.
        let render = Arc::new(move |world: &mut World, target: Entity| {
            let scene = scene_fn(world, target);
            world.entity_mut(target).apply_scene(scene)
        });
        Self {
            deps: deps.into_iter().collect::<Vec<_>>().into(),
            render,
            mode,
        }
    }
}

/// A reactive BSN fragment instance. Attach to an entity (or declare inline
/// with [`reactive`]); whenever a dependency changes, the scene function
/// re-runs and its output is (re-)applied to that entity.
#[derive(Component)]
pub struct Reactor {
    pub(crate) spec: ReactorSpec,
    pub(crate) last_run: Tick,
    pub(crate) state: Vec<DepState>,
}

impl Reactor {
    pub fn patch<S, F>(deps: impl IntoIterator<Item = Dep>, scene_fn: F) -> Self
    where
        S: bevy::scene::Scene,
        F: Fn(&World, Entity) -> S + Send + Sync + 'static,
    {
        Self::from_spec(ReactorSpec::patch(deps, scene_fn))
    }

    pub fn rebuild<S, F>(deps: impl IntoIterator<Item = Dep>, scene_fn: F) -> Self
    where
        S: bevy::scene::Scene,
        F: Fn(&World, Entity) -> S + Send + Sync + 'static,
    {
        Self::from_spec(ReactorSpec::rebuild(deps, scene_fn))
    }

    /// Fork a fresh instance of a (possibly shared) spec.
    pub fn from_spec(spec: ReactorSpec) -> Self {
        let state = spec.deps.iter().map(|_| DepState::default()).collect();
        Self {
            spec,
            last_run: Tick::new(0),
            state,
        }
    }
}

// ---------------------------------------------------------------------------
// Keyed lists
// ---------------------------------------------------------------------------

type KeyedScenes = Vec<(u64, Box<dyn bevy::scene::Scene>)>;
type ItemsFn = dyn Fn(&World) -> KeyedScenes + Send + Sync;

/// Shared, immutable part of a [`ReactorList`].
#[derive(Clone)]
pub struct ReactorListSpec {
    pub(crate) deps: Arc<[Dep]>,
    pub(crate) items: Arc<ItemsFn>,
}

/// A reactive, keyed list of child scenes. The list reconciles **membership
/// and order only**: children with vanished keys despawn, new keys spawn,
/// surviving keys are left untouched (their content updates itself via
/// embedded [`reactive`] fragments). The list owns *all* children of its
/// entity — don't mix in other children.
#[derive(Component)]
pub struct ReactorList {
    pub(crate) spec: ReactorListSpec,
    pub(crate) last_run: Tick,
    pub(crate) state: Vec<DepState>,
    pub(crate) spawned: Vec<(u64, Entity)>,
}

impl ReactorList {
    pub fn new<F>(deps: impl IntoIterator<Item = Dep>, items: F) -> Self
    where
        F: Fn(&World) -> KeyedScenes + Send + Sync + 'static,
    {
        Self::from_spec(ReactorListSpec {
            deps: deps.into_iter().collect::<Vec<_>>().into(),
            items: Arc::new(items),
        })
    }

    pub fn from_spec(spec: ReactorListSpec) -> Self {
        let state = spec.deps.iter().map(|_| DepState::default()).collect();
        Self {
            spec,
            last_run: Tick::new(0),
            state,
            spawned: Vec::new(),
        }
    }
}

/// Convenience for building [`ReactorList`] items out of `bsn!` fragments.
pub fn keyed(
    key: u64,
    scene: impl bevy::scene::Scene + 'static,
) -> (u64, Box<dyn bevy::scene::Scene>) {
    (key, Box::new(scene))
}

// ---------------------------------------------------------------------------
// Inline (bsn-composable) forms
// ---------------------------------------------------------------------------

/// An inline reactive fragment, usable anywhere a `Scene` is expected —
/// including inside `bsn!` trees via `{ reactive(...) }`. Each spawn of the
/// surrounding scene forks a fresh reactor instance on that entity.
/// Use [`Dep::this`] to depend on components of the entity itself.
pub fn reactive<S, F>(deps: impl IntoIterator<Item = Dep>, scene_fn: F) -> impl bevy::scene::Scene
where
    S: bevy::scene::Scene,
    F: Fn(&World, Entity) -> S + Send + Sync + 'static,
{
    from_spec_scene(ReactorSpec::patch(deps, scene_fn))
}

/// Inline form of [`Reactor::rebuild`].
pub fn reactive_rebuild<S, F>(
    deps: impl IntoIterator<Item = Dep>,
    scene_fn: F,
) -> impl bevy::scene::Scene
where
    S: bevy::scene::Scene,
    F: Fn(&World, Entity) -> S + Send + Sync + 'static,
{
    from_spec_scene(ReactorSpec::rebuild(deps, scene_fn))
}

pub(crate) fn from_spec_scene(spec: ReactorSpec) -> impl bevy::scene::Scene {
    // `template` produces a Scene for any closure returning a Component;
    // every template build (= every spawn) forks a fresh instance.
    template(move |_ctx| Ok(Reactor::from_spec(spec.clone())))
}

/// Inline form of [`ReactorList`].
pub fn reactive_list<F>(deps: impl IntoIterator<Item = Dep>, items: F) -> impl bevy::scene::Scene
where
    F: Fn(&World) -> KeyedScenes + Send + Sync + 'static,
{
    let spec = ReactorListSpec {
        deps: deps.into_iter().collect::<Vec<_>>().into(),
        items: Arc::new(items),
    };
    template(move |_ctx| Ok(ReactorList::from_spec(spec.clone())))
}

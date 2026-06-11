//! Async resources: the `createResource` / React Query pattern, ECS-style.
//!
//! [`reactive_async`] ties an async computation to declared dependencies:
//! when a dependency changes, the compute closure runs and its future is
//! spawned on the [`AsyncComputeTaskPool`]; the result lands as an ordinary
//! [`AsyncValue<T>`] **component**, which the render fragment watches like
//! any other ECS state. There is no async runtime inside the reactive layer
//! — just a task handle driven to completion by a small system.
//!
//! Semantics — chosen for a retained-mode ECS, with the web equivalents
//! noted where they exist:
//! - **Fallbacks are just rendering** (cf. Suspense): the render closure
//!   receives [`AsyncValue::Pending`] before the first result and returns
//!   whatever fallback it likes.
//! - **Last good value by default** (cf. stale-while-revalidate): when
//!   dependencies change while a value is present, the old `Ready` keeps
//!   rendering until the new result lands — no fallback flicker. The render
//!   closure also receives [`AsyncView::refreshing`], so stale values can
//!   be *marked* stale (dimmed, spinner, …) instead of trusted silently.
//! - **Correctness-critical consumers should tag, not trust.** Last-good-
//!   value is right for ambient displays; when acting on a result requires
//!   knowing *which request* produced it, embed the request in `T` and
//!   check it (the validation game's pathfinding tags results with the job
//!   entity) — or use [`AsyncSlot`] directly.
//! - **Cancellation:** re-launching replaces the in-flight task handle;
//!   dropping a Bevy [`Task`] cancels it, so superseded computations can't
//!   deliver out-of-order results.

use std::future::Future;

use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};

use crate::{Dep, ReactorSpec, from_spec_scene};

/// The state of an async computation, stored as an ordinary component on
/// the [`reactive_async`] entity. Reactivity sees it through normal deps.
#[derive(Component, Debug)]
pub enum AsyncValue<T: Send + Sync + 'static> {
    /// No result has arrived yet (only observed before the first result).
    Pending,
    /// The most recent completed result.
    Ready(T),
}

impl<T: Send + Sync + 'static> AsyncValue<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            AsyncValue::Ready(value) => Some(value),
            AsyncValue::Pending => None,
        }
    }
}

type PollFn = dyn FnMut(&mut World, Entity) -> bool + Send + Sync;

/// An in-flight task, type-erased so one driver system serves every value
/// type. Replacing the slot drops (= cancels) the previous task.
///
/// [`reactive_async`] uses this internally, but it stands alone: insert one
/// on any entity and the future's output lands there as
/// [`AsyncValue<T>::Ready`], to be consumed by plain systems just as well
/// as by reactors — async results are ordinary ECS state either way.
///
/// ```ignore
/// // e.g. request a path; a movement system reads AsyncValue<PathResult>.
/// commands.entity(beaver).insert((
///     AsyncValue::<PathResult>::Pending,
///     AsyncSlot::new(async move { find_path(grid, from, to) }),
/// ));
/// ```
#[derive(Component)]
pub struct AsyncSlot {
    poll: Box<PollFn>,
}

impl AsyncSlot {
    /// Spawn `future` on the [`AsyncComputeTaskPool`]; when it completes,
    /// its output is inserted on the owning entity as [`AsyncValue::Ready`].
    /// Inserting a new slot replaces (= cancels) any in-flight one.
    pub fn new<T, Fut>(future: Fut) -> Self
    where
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let mut task: Option<Task<T>> = Some(AsyncComputeTaskPool::get().spawn(future));
        AsyncSlot {
            poll: Box::new(move |world: &mut World, entity: Entity| {
                let Some(running) = task.as_mut() else {
                    return true;
                };
                match block_on(poll_once(running)) {
                    Some(value) => {
                        task = None;
                        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                            entity_mut.insert(AsyncValue::Ready(value));
                        }
                        true
                    }
                    None => false,
                }
            }),
        }
    }
}

/// Polls in-flight tasks; on completion, writes the typed [`AsyncValue`]
/// onto the owning entity (waking whatever watches it) and removes the slot.
/// Runs before [`ReactSet`](crate::ReactSet) so results render same-frame.
pub(crate) fn drive_async_slots(world: &mut World) {
    let entities: Vec<Entity> = world
        .query_filtered::<Entity, With<AsyncSlot>>()
        .iter(world)
        .collect();
    for entity in entities {
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            continue;
        };
        let Some(mut slot) = entity_mut.take::<AsyncSlot>() else {
            continue;
        };
        let done = (slot.poll)(world, entity);
        if !done && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.insert(slot);
        }
    }
}

/// What a [`reactive_async`] render closure sees: the (possibly stale)
/// value, plus whether a recomputation is currently in flight.
pub struct AsyncView<'a, T: Send + Sync + 'static> {
    pub value: &'a AsyncValue<T>,
    /// A newer computation is running; a `Ready` value is the *previous*
    /// result. While true, the fragment re-renders every frame (the task
    /// handle ticks as it is polled), so keep refreshing-state rendering
    /// cheap.
    pub refreshing: bool,
}

impl<'a, T: Send + Sync + 'static> AsyncView<'a, T> {
    pub fn ready(&self) -> Option<&'a T> {
        self.value.ready()
    }
}

/// An async reactive fragment, usable anywhere a `Scene` is expected.
///
/// When any of `deps` changes, `compute` builds a future from current world
/// state; the future runs on the [`AsyncComputeTaskPool`] and its output is
/// stored as [`AsyncValue<T>`] on this entity. `render` (on a child
/// fragment) re-runs whenever that value changes, receiving
/// `Pending`-before-first-result and `Ready` thereafter — old values persist
/// while a recomputation is in flight, with [`AsyncView::refreshing`]
/// telling the renderer so.
///
/// ```ignore
/// reactive_async(
///     [Dep::resource::<SelectedPlayer>()],
///     |world: &World, _| {
///         let id = world.resource::<SelectedPlayer>().0;
///         async move { fetch_profile(id).await }
///     },
///     |_world, _entity, profile: AsyncView<Profile>| match profile.ready() {
///         None => bsn! { Text("loading…") },
///         Some(p) => bsn! { Text({ p.name.clone() }) },
///     },
/// )
/// ```
pub fn reactive_async<T, Fut, FCompute, S, FRender>(
    deps: impl IntoIterator<Item = Dep>,
    compute: FCompute,
    render: FRender,
) -> impl bevy::scene::Scene
where
    T: Send + Sync + 'static,
    Fut: Future<Output = T> + Send + 'static,
    FCompute: Fn(&World, Entity) -> Fut + Send + Sync + Clone + 'static,
    S: bevy::scene::Scene,
    FRender: for<'a> Fn(&World, Entity, AsyncView<'a, T>) -> S + Send + Sync + 'static,
{
    // Launcher: a patch reactor on this entity whose scene is a template
    // that (re)spawns the task. Everything it writes — the slot, the
    // pending marker — lands on its own entity, honoring the write contract.
    let launcher = ReactorSpec::patch(deps, move |_: &World, _: Entity| {
        let compute = compute.clone();
        bevy::ecs::template::template(move |ctx| {
            let entity = ctx.entity.id();
            let future = compute(ctx.entity.world(), entity);
            if ctx.entity.get::<AsyncValue<T>>().is_none() {
                ctx.entity.insert(AsyncValue::<T>::Pending);
            }
            Ok(AsyncSlot::new(future))
        })
    });

    // Renderer: a child fragment watching the parent's AsyncValue, plus the
    // task slot so refreshing-state changes re-render too.
    let renderer = ReactorSpec::patch(
        [Dep::parent::<AsyncValue<T>>(), Dep::parent::<AsyncSlot>()],
        move |world: &World, child: Entity| {
            let pending = AsyncValue::<T>::Pending;
            let parent = world.get::<ChildOf>(child).map(|c| c.parent());
            let value = parent
                .and_then(|p| world.get::<AsyncValue<T>>(p))
                .unwrap_or(&pending);
            let refreshing = parent.is_some_and(|p| world.get::<AsyncSlot>(p).is_some())
                && value.ready().is_some();
            render(world, child, AsyncView { value, refreshing })
        },
    );

    let launcher = from_spec_scene(launcher);
    let renderer = from_spec_scene(renderer);
    bsn! {
        { launcher }
        Children [ ( { renderer } ) ]
    }
}

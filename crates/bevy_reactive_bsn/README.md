# bevy_reactive_bsn

Fine-grained, change-tick-driven reactivity for Bevy's BSN scene system
(`bsn!`, Bevy 0.19+) — an experiment in the design space Cart sketched for
"Reactive BSN" in [bevy#14437], built entirely on public APIs.

```rust
use bevy::prelude::*;
use bevy_reactive_bsn::{reactive, Dep, ReactiveBsnPlugin};

#[derive(Resource, Default)]
struct Score(u32);

fn hud() -> impl Scene {
    bsn! {
        Node { padding: UiRect::all(px(8)) }
        Children [
            // A reactive fragment is just a scene function: whenever `Score`
            // changes, the closure re-runs and its patch is re-applied to
            // this entity, in place.
            reactive([Dep::resource::<Score>()], |world: &World, _: Entity| {
                let score = world.resource::<Score>().0;
                bsn! { Text({ format!("Score: {score}") }) }
            }),
        ]
    }
}
```

No signals, no wrappers, no shadow tree: reactive state is **ordinary ECS
state**, dirtiness comes from Bevy's native change ticks, and updates are
BSN patches merged onto live entities.

## Design

Each decision below traces to a failure mode of an earlier Bevy reactivity
attempt (bevy_reactor/quill, bevy_cobweb, kayak_ui, belly, jonmo/haalka):

- **Reactive state is the ECS.** Anything any system writes — with no special
  wrapper — can drive a reactor. (`React<T>` wrappers fork the component
  vocabulary and silently miss unwrapped writes; signal cells move state
  where queries, reflection and inspectors can't see it.)
- **Declared dependencies, not implicit read-tracking.** A reactor states
  what it watches. This maps 1:1 onto change detection, is trivially
  debuggable, and avoids the hook-ordering footguns implicit tracking brings
  to Rust.
- **Updates are patches, not diffs.** A dirty reactor re-runs its scene
  function and `apply_scene`s the result onto its own entity: component
  patches merge in place, so focus/hover/animation state and components
  owned by other systems survive. There is no virtual tree to reconcile.
- **Ownership is the entity graph.** A reactor is a component; despawning the
  entity tears down everything it manages. Nothing to unsubscribe.
- **Dynamic children are explicit**, because BSN's `apply_scene` re-spawns
  `Children [..]` on every application. Three primitives, by structure:
  - `reactive(deps, f)` / `Reactor::patch` — childless fragments, merged in
    place (text, colors, transforms, materials);
  - `reactive_rebuild(deps, f)` / `Reactor::rebuild` — replaces its subtree
    when *structure* depends on state;
  - `reactive_list(deps, items)` / `ReactorList` — keyed collections. The
    list reconciles **membership and order only**: vanished keys despawn,
    new keys spawn, survivors are left untouched — item content updates
    itself via embedded `reactive` fragments.

### Dependencies

| constructor | wakes on |
|---|---|
| `Dep::resource::<R>()` | resource changed / inserted / removed |
| `Dep::this::<T>()` | `T` on the reactor's own entity (what inline fragments use) |
| `Dep::entity::<T>(e)` | `T` on a specific entity |
| `Dep::parent::<T>()` | `T` on the entity's `ChildOf` parent — re-parenting wakes it too |
| `Dep::presence::<T>(e)`, `presence_this` | insert/remove of `T` only, mutations ignored |
| `Dep::components::<T>()` (`_filtered`) | `T` on *any* entity, incl. population changes |
| `Dep::related::<S>(e)`, `related_this` | the relation set `S` (e.g. `Children`) of `e` |
| `Dep::related_components::<S, T>(e)`, `…_this` | `T` on any entity related to `e` via `S` |

`Dep`s are pure, `Arc`-cheap specifications; per-instance state lives on the
reactor, so a `ReactorSpec` can be forked across thousands of entities
(`Reactor::from_spec`) — one allocation, many instances.

### Scheduling

Reactors run in one exclusive system (in `Update`, set `ReactSet`), looping
until no reactor is dirty so chains settle within a single frame, with a
pass cap and a divergence warning. Three properties keep this cheap:

1. **Shared scans.** Whole-world deps (`Dep::components`) are answered by one
   scan per component type per pass, however many reactors watch that type.
2. **Written-entity pass filtering.** A reactor's render is expected to write
   components only on its own target entity and its (de)spawned descendants
   (resource writes are fine — resource deps are re-checked every pass).
   Follow-up passes therefore skip every reactor whose entity-targeted deps
   point outside the previous pass's written set; what remains (resource
   deps, shared type scans) is O(1)-ish to re-check. **Debug builds enforce
   the contract** (sampled ~1-in-16 frames — realistic violations are
   systematic, so sampling catches them within a second at negligible
   cost; release builds compile the checker out entirely): the runner
   sweeps change ticks
   and loudly logs any component written outside a rendering reactor's
   subtree (including sneaky cases like inserting `ChildOf(other)`, which
   mutates `other`'s `Children`). Observer-machinery bookkeeping is exempt:
   executing an observer takes `&mut Observer`, so observers that fire in
   response to legitimate subtree writes tick their own component — that is
   not game state, but anything those observers *write* is still checked.
   Crucially, the contract is load-bearing for *same-frame convergence*,
   not correctness: every frame's first pass checks all reactors
   unconditionally, so a violation degrades to a one-frame delay for the
   affected watcher (pinned by a test), never a lost update.
3. **Tick hygiene.** Stored ticks are clamped against `MAX_CHANGE_AGE`, so
   sessions running for days can't wrap into false wakes.

Wake-ups are traced: `RUST_LOG=reactive_bsn=debug` logs which dependency
woke which reactor, per pass.

### Numbers

Headless micro-benchmarks against a hand-written `Changed<T>` system
(release, Ryzen 7 5800X; harness in the BeaverBuilds repo, `BB_BENCH=1`):

| scenario | ms/frame | per unit |
|---|---|---|
| 10k patch reactors (`Dep::this`), idle | 0.42 | 42 ns/reactor |
| 10k patch reactors, 100 dirty/frame | 0.50 | ~0.8 µs/update |
| 10k patch reactors, all dirty | 3.2 | ~280 ns/apply |
| 1k `Dep::components` watchers over 10k entities, idle | 0.11 | one shared scan |
| baseline `Changed<T>` system, 10k entities | 0.06 | ~6 ns/entity |

The idle check is ~7× the raw ECS floor — the price of a dynamic layer; at
UI scale (a few thousand reactors) it is fractions of a millisecond.

## Async resources

`reactive_async` is the `createResource` / React Query pattern, ECS-style:
when declared deps change, a compute closure builds a future (run on the
`AsyncComputeTaskPool`); the result lands as an ordinary `AsyncValue<T>`
**component**, which the render fragment watches like any other state.

```rust
reactive_async(
    [Dep::resource::<SelectedPlayer>()],
    |world: &World, _| {
        let id = world.resource::<SelectedPlayer>().0;
        async move { fetch_profile(id).await }
    },
    |_, _, profile: &AsyncValue<Profile>| match profile.ready() {
        None => /* fallback — this *is* Suspense */,
        Some(p) => /* content */,
    },
)
```

The web concepts map cleanly because the result is just ECS state:
**Suspense** is the render arm for `Pending`; **stale-while-revalidate /
transitions come for free** — retained-mode UI plus in-place patching means
the old `Ready` value (and the old UI) persists until the new result lands,
with `Pending` only ever observed before the first result; **cancellation**
is dropping the replaced Bevy `Task`, so stale computations can't deliver
out-of-order. No async runtime lives inside the layer — one small system
drives task handles to completion. (Bevy's in-flight `bevy_async` sync-point
bridge, PR #21744, composes with this rather than replacing it: bridges give
async tasks *ECS access*; `reactive_async` gives reactive scenes *async
values*.)

## Compared to React / SolidJS

Philosophically this is Solid, not React: fragments run once, updates mutate
the target directly, structure changes are explicit (`reactive_rebuild` /
`reactive_list` ≈ `<Show>` / `<For>`), and there is no VDOM diff. It departs
from Solid deliberately: dependencies are declared rather than read-tracked,
granularity is the resource/component rather than the field, and propagation
is pull-batched per frame rather than push-per-write — ECS writes go through
`DerefMut`, not setters, so there is nothing to push from, and a game frame
is the natural batch boundary anyway.

## Limitations / future work

- **Per-field invalidation** ("only `Score.lives` feeds this patch") needs
  field-level metadata in change detection and resolved BSN patches —
  upstream work by design; this layer stays on public APIs.
- One `Reactor` + one `ReactorList` per entity (wrap extra fragments in
  child entities).
- The runner is single-threaded (exclusive system); dep checks are
  embarrassingly parallel if that ever matters.
- Reactor scenes must only write their own entity and (de)spawned
  descendants (checked in debug builds; see Scheduling above).
- Runs in `Update` by default; state written later (e.g. `PostUpdate`)
  is picked up next frame.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option — the same terms as Bevy itself. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in the
work by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.

[bevy#14437]: https://github.com/bevyengine/bevy/discussions/14437

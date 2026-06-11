# Reactive BSN: a change-tick-driven reactivity layer — design report

> **AI disclosure:** this document and the implementation it describes were
> written by an AI (Anthropic's Claude), directed and reviewed by a human.
> Per [Bevy's AI policy](https://bevy.org/learn/contribute/policies/ai/),
> none of this is submitted as a contribution and the code is not mergeable
> into Bevy Org repositories. It is published as *evidence from the design
> space* opened by the "Reactive BSN" experimentation phase called for in
> [#14437] — one implementation's worth of data about what works, for humans
> to evaluate, re-derive, and hand-author if any of it proves useful. No
> side is taken in the AI-contribution debate.

**Status:** working implementation on Bevy 0.19 public APIs, validated in a
full game (UI *and* world entities), with behavioral tests and benchmarks.
This design space was called for in [#14437] and deferred from [#23413].

## TL;DR

We built and shipped a reactivity layer over BSN with **no signals, no
wrappers, and no shadow tree**: reactors are components carrying declared
ECS dependencies plus a scene function; dirtiness comes from native change
ticks; updates are BSN patches re-applied in place. It needs **zero changes
to Bevy** — `bsn!` already composes reactive fragments as ordinary
scene-function includes — and it surfaces exactly one capability that only
upstream can provide: per-field invalidation. Everything else in #14437's
reactivity sketch is implementable, today, on the public API.

## Goals

1. Reactive UI *and* reactive world entities (the layer should be one
   mechanism, not a UI framework).
2. Plain ECS state as the only source of truth — anything any system writes
   can drive a reactor, with no opt-in wrapper.
3. Incremental updates that respect live entities: no respawns for value
   changes, no diffing against state other systems own.
4. Deterministic, in-schedule execution; chains settle within one frame.
5. Costs proportional to change, with a measurable, low idle floor.

## Non-goals

- Per-field invalidation (needs upstream metadata; see "What needs Bevy").
- Implicit read-tracking (a deliberate rejection, argued below).
- Replacing systems: continuous per-frame work (movement, simulation)
  remains plain systems; reactivity is for state → derived-state edges.

## The core abstraction

```rust
// Inline in a scene — each spawn forks a fresh instance:
bsn! {
    Node Children [
        reactive([Dep::resource::<Score>()], |world: &World, _: Entity| {
            let score = world.resource::<Score>().0;
            bsn! { Text({ format!("Score: {score}") }) }
        }),
    ]
}

// Or attached to an existing entity:
commands.entity(sun).insert(Reactor::patch([Dep::resource::<Season>()], …));
```

A `Reactor` is a component holding any number of independent *fragments*
(compose with `Reactor::and`, or by applying scenes sequentially — inline
fragments merge). Each fragment holds:
- a list of **declared dependencies** (`Dep`) — pure, `Arc`-shared specs;
- a **scene function** `Fn(&World, Entity) -> impl Scene`;
- a `last_run: Tick` and per-dep instance state.

A fragment is *dirty* when any of its dependencies reports a change tick
newer than its `last_run`. When dirty, the scene function re-runs and the result is
re-applied to the reactor's entity via `apply_scene` — BSN's patch semantics
merge component values in place, which is what makes "re-render" cheap and
non-destructive. The closure boundary type-erases the unnameable `bsn!`
types, so the component stays object-safe and forkable.

Three primitives, separated by what they do to *structure* (because
`apply_scene` re-spawns `Children [..]` on every application, structural
updates must be explicit — we consider this a feature, not a bug):

| primitive | structure | use |
|---|---|---|
| `reactive` / `Reactor::patch` | none (childless fragment) | text, colors, transforms, materials |
| `reactive_rebuild` / `Reactor::rebuild` | replaces its subtree | conditional panels, mode switches |
| `reactive_list` / `ReactorList` | keyed membership + order only | collections; item content self-updates via embedded `reactive` |

The list never re-applies surviving items — the React-keys insight without
the reconciliation: vanished keys despawn, new keys spawn, survivors are
untouched.

A fourth, *derived* primitive shows the model extends to async:
`reactive_async(deps, compute, render)` — deps change → a future runs on the
`AsyncComputeTaskPool` → the result lands as an ordinary `AsyncValue<T>`
component → a child fragment (watching it via `Dep::parent`) renders it.
Suspense is the `Pending` render arm; stale-while-revalidate is the default
(retained-mode UI keeps the old value rendered until the new result lands);
cancellation is dropping the replaced task handle. Notably, the layer's own
write contract dictated the shape: the launcher lives on the parent and the
renderer on a child, because a child must not write its parent.

### Dependencies

| constructor | wakes on |
|---|---|
| `Dep::resource::<R>()` | resource change/insert/remove |
| `Dep::resource_value(\|r: &R\| …)` / `Dep::this_value` | a *projection* changed value (tick-gated; per-field wakes today) |
| `Dep::this::<T>()` / `Dep::entity::<T>(e)` | `T` on one entity, incl. insert/remove |
| `Dep::presence::<T>(e)` | insert/remove only (pair a rebuild-on-presence with a patch-on-value) |
| `Dep::parent::<T>()` | `T` on the entity's `ChildOf` parent (child fragments rendering parent-owned state) |
| `Dep::components::<T>()` | `T` anywhere, incl. population changes |
| `Dep::related::<S>(e)` / `Dep::related_components::<S, T>(e)` | relation set / components across a relation |

**Why declared, not tracked?** Cart's sketch leans toward observer-style
fine granularity; every prior implicit-tracking implementation in Rust
(quill's `Cx` scopes most prominently) ended up with hook-ordering rules,
panics on conditional reads, and opaque dependency graphs. Declared deps
are slightly more verbose, map 1:1 onto change detection, are introspectable
(the runner traces "reactor X woken by resource Season" per pass), and cost
nothing to get wrong in a *visible* way (a missing dep = visibly stale UI,
not a heisenbug). We think this is the right trade for Bevy even long-term;
implicit tracking can be layered on later without changing the model.

## Scheduling

One exclusive system; loops until no reactor is dirty (cap + divergence
warning), so reactor→reactor chains settle in a single frame. Three
mechanisms keep it cheap:

1. **Shared scans** — `Dep::components::<T>()` is answered by one scan of
   `T` per pass regardless of watcher count.
2. **Written-entity pass filtering** — the key idea. Applying a scene can
   only write components on the reactor's own entity and its (de)spawned
   descendants. Follow-up convergence passes therefore skip any reactor
   whose entity-targeted deps point outside the set of entities written by
   the previous pass. Resource deps don't need the argument (always O(1)
   re-checked), which keeps the contract small. The contract, its precise
   scope, its enforcement, and its graceful failure mode get their own
   subsection below.
3. **Tick hygiene** — stored ticks are clamped against `MAX_CHANGE_AGE`,
   mirroring `check_change_ticks`.

### The write contract, precisely

Because the pass filter leans on it, the contract deserves exact terms.

**Statement.** Applying a reactor's scene (including any hooks/observers
that fire in response) may only write components on the reactor's own
entity and entities it (de)spawns beneath itself. Resources are uncovered
(resource deps re-check every pass), as are component types watched only by
whole-world deps (shared scans re-run every pass). So the policed surface
is narrow: *writes to types that some reactor watches on specific entities,
on entities outside the writer's subtree*.

**What it is load-bearing for — and what it is not.** The contract
guarantees *same-frame convergence* of reactor chains and pays for the pass
filter's speed. It is **not** a correctness precondition: pass 0 of every
frame checks all reactors unconditionally, so a violation degrades to a
one-frame delay for the affected watcher — the same observable behavior as
state written after the runner in the schedule. This bound is pinned by a
test (`contract_violation_degrades_to_one_frame_delay`), which deliberately
violates the contract in the worst-case ordering and asserts repair on the
next frame. Lost updates are impossible by construction.

**Relationships, specifically.** Relationship edges are components, which
cuts both ways. Watching *through* the graph is safe by construction:
`Dep::related_components` and `Dep::parent` always re-check in follow-up
passes (their reach can't be resolved against a written set), and
`Dep::parent` treats the `ChildOf` edge as part of the dependency, so pure
re-parenting wakes it. Writing *through* the graph is where the contract
bites: relationship maintenance inside the writer's subtree is covered (the
written set includes rebuild/list targets and all despawned descendants),
but edges that cross the subtree boundary — re-parenting onto an outside
entity, or despawning a child whose relationships point outside — mutate
the far endpoint's collection. Those are contract violations like any
other: flagged by the debug checker when watched, bounded by the one-frame
repair.

**Why it is rarely felt.** Reactor scenes are presentational patches; the
only ways to break the contract are reaching into the world from a template
closure or triggering an observer that writes elsewhere — both of which are
better expressed as systems reacting to the same state. Across the
validation game (~5k reactors, every primitive exercised) the contract was
never violated; the structural pressure it applies even improved a design
(`reactive_async` puts the launcher on the parent and the renderer on a
child precisely because a child must not write its parent).

**Enforcement.** Debug builds sweep change ticks after each active pass and
log out-of-subtree writes, scoped to the watched-type set — which is what
makes the checker precise. Building it surfaced two pieces of engine
bookkeeping that fire on legitimate writes (executing an observer ticks its
own `Observer` component; render-relevant inserts tick render-world sync
markers); watched-type scoping excludes them automatically rather than by
hardcoded exemption.

### Measured (release, Ryzen 7 5800X, vs a hand-written `Changed<T>` system)

| scenario | ms/frame | per unit |
|---|---|---|
| 10k patch reactors, idle | 0.42 | 42 ns/reactor |
| 10k patch reactors, 100 dirty/frame | 0.50 | ~0.8 µs/update |
| 10k patch reactors, all dirty | 3.2 | ~280 ns/apply |
| 1k whole-world watchers over 10k entities, idle | 0.11 | one shared scan |
| 10k value-projection deps, noisy resource, stable value | 0.48 | ≈ idle floor |
| baseline `Changed<T>` system, 10k entities | 0.06 | ~6 ns/entity |

The idle check is ~7× the raw ECS floor; at realistic scales (a few
thousand reactors) the layer is fractions of a millisecond. The interesting
shape: cost is dominated by the *pull* pass over reactors, which is the
fundamental consequence of Bevy's change detection being pull-only — there
is no push channel for mutations, and we argue below there shouldn't be.

## Validation

- **A full game** (BeaverBuilds, a Timberborn-like) uses the layer for its
  entire HUD *and* its world: per-tile water visuals, irrigation tinting,
  tree growth, construction progress (a rebuild-on-presence containing a
  nested patch-on-value), drought lighting and fog, a placement ghost. ~5k
  reactors ≈ 0.2 ms idle. Its drought-forecast readout is an async resource:
  the water simulation is run through an entire drought on the task pool
  (every few seconds, or when a dam appears) and the retention percentage
  renders reactively. Continuous motion and the live water cellular
  automaton remain plain systems — the boundary held up well in practice.
- **31 behavioral tests** (headless) pin the semantics: exactly one run per
  change (no spurious wakes), in-place merging preserves foreign components,
  presence ignores mutations, population changes wake whole-world deps,
  rebuild despawns old subtrees, list survivors keep their entities, chains
  settle in one frame *through* the pass filter, shared specs fork
  independent instances, teardown-by-despawn, self-dirtying reactors
  terminate, async resources render pending-then-ready, stale values persist
  while a recomputation is in flight, and a deliberate contract violation
  degrades to exactly a one-frame delay, `Dep::parent` wakes on pure
  re-parenting and orphaning (the `ChildOf` edge is itself part of the
  dependency), relationship deps track member mutation/addition/removal/
  despawn, filtered scans ignore non-matching entities, rebuilt subtrees'
  brand-new nested reactors settle the same frame, list reorders follow key
  order with surviving entities, duplicate list keys collapse with a
  warning, and a second inline reactor on one entity fails loudly at spawn
  (duplicate-component panic) rather than silently replacing the first.
  Value projections wake only on projected-value change and provably skip
  projecting while the source is quiet (tick-gating); multiple fragments on
  one entity wake and re-apply strictly independently, and sequential scene
  applications merge fragments with documented append/replace identity.

## What this validates from #14437 — and what it challenges

**Validated:** fine-grained reactivity composes additively with the ECS;
change ticks suffice as the dirty source; BSN's repeatable templates +
patch merging are exactly the right substrate for in-place reactive
updates; dynamic children are the hard part and benefit from being
*explicit* (rebuild vs keyed list) rather than inferred.

**Challenged, gently:** "observer-style" need not mean push-per-write.
Mutations in Bevy have no push channel (writes go through `DerefMut`), and
adding one (setter wrappers) was bevy_cobweb's mistake. Frame-batched pull
over declared deps gets the same observable granularity, batches naturally
(as Solid batches via microtasks), and keeps zero overhead on the write
path. We'd encourage upstream reactivity to stay pull-based.

## What needs Bevy (the actual upstream asks)

1. **Per-field invalidation — a smaller ask than expected.** We started
   from "this is the one thing a layer cannot do," then validated both
   halves of the *capability* on the public API; what remains for upstream
   is ergonomics and constant factors, not capability.
   - **Per-field wakes** are tick-gated value projections:
     `Dep::resource_value(|s: &Season| s.remaining.ceil() as u32)` wakes a
     fragment only when the projected value changes. Quiet sources cost one
     tick compare; noisy sources with stable projections cost one
     projection + `PartialEq` (benchmarked at the idle floor for 10k deps
     over a per-frame-noisy resource). In the validation game, the calendar
     re-renders once per displayed second while the underlying field ticks
     every frame — the simulation writes naturally; no derived resources,
     no `bypass_change_detection`.
   - **Per-field re-application** is fragment splitting: an entity carries
     any number of independent fragments (`Reactor::and`, or merged via
     sequential scene application), so "re-apply only the dirty field"
     becomes "one small fragment per concern, each with its own projection
     dep." A test pins two fragments on one entity waking and re-applying
     strictly independently.
   - **The remaining upstream experiment** — unvalidatable from outside by
     nature — is removing what the layer-level emulation costs: field-level
     dirty metadata (eliminates the projection cache and compare), lens
     inference (eliminates the hand-written projection closures), and
     sub-patch diffing (eliminates the fragment-splitting boilerplate).

   That the layer gained both halves without touching anything else is
   itself evidence that the rest of the design is agnostic to how fine the
   dirty bits get.
2. *(Optional)* a `bsn!`-native reactive entry. We expected to need one; we
   didn't — scene-function includes already compose `reactive(...)`
   naturally, with full IDE support. A dedicated syntax would only add
   field-patch sugar (e.g. reactive expressions in field position), which
   collapses into per-field invalidation anyway.
3. *(Nice-to-have)* a public, stable way to iterate resource/component
   change ticks for tooling — the debug contract checker and wake tracer
   would get cheaper and richer.

## Relationship to in-flight upstream work (June 2026)

- **Mutation observers are shelved, and that supports this design.** The
  `OnMutate` lineage ([#14520] → [#16143] → [#16183]) is closed unmerged —
  the last one in May 2026 — with the recurring objection (viridia) that
  mutation observers are "algorithmically expensive and likely to cause
  performance regressions… even if you don't use the feature". This layer
  is the counter-experiment: frame-batched pull over declared deps delivers
  observer-grained reactivity with zero write-path cost.
- **The funded direction makes this layer faster, not obsolete.** Project
  Goal [#23152] (*Fast and Flexible Change Detection*) and [#23519]
  (opt-in per-entity-page change indexes, targeting the 0.19 mega-worlds
  push) accelerate *pull*. When change indexes land, this layer's shared
  per-type scans can ride them directly: whole-world dependency checks drop
  from O(entities with `T`) to O(changed pages) with no API change here.
- **`bevy_async` ([#21744], sync-point bridges) composes naturally.** The
  reactor runner's position in the schedule is exactly the kind of sync
  point async tasks would bridge into (e.g. await settled UI state).
  Notably, the same author's experimental async BSN UI implements
  `on_mutation` by *pumping `Changed<C>` scans at sync points* — convergent
  evolution: everyone building reactivity on today's Bevy ends up
  pull-based, which is an argument for standardizing the substrate (shared
  scans, declared deps) rather than each layer re-rolling it.
- **Web-style async reactivity needs no new machinery.** This crate's
  `reactive_async` (a `createResource`/React-Query analog: deps → future →
  result-as-component → render) demonstrates that Suspense is just the
  `Pending` render arm, and stale-while-revalidate/transition semantics
  fall out of retained-mode UI *by default* — the old value keeps rendering
  until the new one lands, no `startTransition` machinery required. The
  async result being an ordinary component is the load-bearing decision:
  reactivity composes with it through the same deps as everything else —
  and so do plain systems. The validation game exercises both consumers:
  its drought-forecast readout is a `reactive_async` fragment, and its
  beaver pathfinding (A* on the task pool, requested on job claim,
  cancelled by slot replacement, waypoints consumed by the movement
  system) uses the same `AsyncSlot`/`AsyncValue` machinery with no reactor
  involved at all.

## Open questions

- Should the write contract be enforced in release builds behind a feature
  (`strict`), or is the debug sweep enough?
- Parallel dep checking: the per-reactor checks are embarrassingly parallel
  read-only work; worth it past ~50k reactors, not before.
- Multi-schedule runners (e.g. a second pass post-`PostUpdate`) — trivially
  possible, scheduling policy unclear.
- `Dep` granularity for assets (`Dep::resource::<Assets<T>>()` works but is
  collection-wide; per-handle deps would want asset events).

## Appendix: prior-art positioning

bevy_reactor/quill proved fine-grained signals-in-the-World and hybrid
coarse/fine updates, at the cost of hook rules and a nightly feature;
bevy_cobweb proved despawn-reactivity and reaction trees, at the cost of
`React<T>` wrappers; jonmo/haalka prove FRP collections (`SignalVec`) in
ECS clothing, at the cost of an opaque combinator graph; kayak/belly are
cautionary tales about VDOM-diffing live entities and stringly bindings.
This design takes: entities-as-owners (reactor), change-ticks-as-dirty-bits
(everyone's endgame), explicit structure ops (Solid's `<Show>`/`<For>`),
keyed membership without content reconciliation (React keys minus VDOM),
and adds the written-set convergence argument, which we believe is novel.

[#14437]: https://github.com/bevyengine/bevy/discussions/14437
[#23413]: https://github.com/bevyengine/bevy/pull/23413
[#14520]: https://github.com/bevyengine/bevy/pull/14520
[#16143]: https://github.com/bevyengine/bevy/pull/16143
[#16183]: https://github.com/bevyengine/bevy/pull/16183
[#23152]: https://github.com/bevyengine/bevy/issues/23152
[#23519]: https://github.com/bevyengine/bevy/pull/23519
[#21744]: https://github.com/bevyengine/bevy/pull/21744

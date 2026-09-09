# fbui-elm design: the Elm architecture on a retained tree

> This is the mini-plan PLAN §4 (Phase 7) promises for the "declarative UI
> layer" backlog item. Like `fbui-widgets/DESIGN.md` it is the phase's first
> task: it fixes the program model, the view tree, the reconciler, and the
> effect system before any code is written, and it lists the changes the
> existing crates need. Status: **designed, not implemented.**

## 0. One-paragraph summary

`fbui-elm` adds a `Program` — `init` / `update(&mut Model, Msg) -> Cmd` /
`view(&Model) -> Node<Msg>` / `subscriptions(&Model) -> Sub<Msg>` — on top of
the existing retained `Ui<Msg>`. The app describes the whole screen as a value
each time the model changes; a **reconciler** diffs that description against the
retained tree and applies the difference through a new `Ui::patch` plus
`add_child` / `remove`, so the damage tracker sees exactly the pixels that
changed and nothing else. Widgets keep their internal state (scroll offsets,
carets, tweens) across renders because they are never recreated while their
identity holds. Effects are data (`Cmd`, `Sub`) executed by the runtime through
the existing `Proxy` / `Timer` machinery, which makes a whole program testable
headless: send messages, assert on the model, assert on the recorded effects,
snapshot the pixels. The retained `App` API stays; the two coexist, and an Elm
program can embed a retained widget as an *island* where it needs one.

## 1. Why, and what must not be lost

### 1.1 What the retained API costs application code

Today an app is `App::build` + `App::update`, and `update` pushes state into
widgets by id:

```rust
fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
    match msg { Msg::Inc => self.value += 1, Msg::Dec => self.value -= 1 }
    if let Some(id) = self.label {                       // an Option<WidgetId> per widget
        ui.with::<Label, _>(id, |l| l.set_text(self.value.to_string()));
    }
}
```

This is fine for a counter and gets worse linearly with the app: every piece of
state that appears on screen needs a stored id, a `with` call in the right
branch, and discipline to keep the widget and the model in sync. Structure is
the same story — opening a dialog is five `add_child` calls plus a
`focus_first`, closing it is a `remove`, and forgetting either leaves the tree
and the model disagreeing. Nothing checks that agreement; the failure mode is a
screen that shows stale state.

### 1.2 What Elm gives

One source of truth (the model), a view that is a pure function of it, and
effects as values. The screen cannot disagree with the model because the screen
*is* `view(model)`. Testing collapses to "send messages, look at the model and
the effects". This is the architecture the `fbui-widgets` lib docs already call
the control loop "Elm-ish" after; this layer completes it.

### 1.3 What must not be lost

`fbui-widgets/DESIGN.md §1` decided *against* rebuilding the view every update,
for the reason this framework exists: a Pi-class CPU cannot repaint 1080p per
click. That decision stands. Every invariant in `CLAUDE.md` is inherited
unchanged:

| Invariant | What it means for this layer |
|---|---|
| Idle burns ~0% CPU | A render that changes nothing must produce **no damage**. `view` may run; the reconciler must not touch the `Ui` unless a prop actually changed. |
| Repaint only what changed | Reconcile → `Ui::with` on exactly the widgets whose props differ; damage stays per-widget. |
| Widgets are state machines, no wall clock | The view describes *props*; widget *state* (scroll, caret, tween) is owned by the retained widget and survives renders. |
| The fast path never diverges from the slow one | Reconciling into an existing tree must produce the same pixels as building the tree fresh from the same view. This is a test (§7.2), like `scroll_blit_matches_a_full_repaint`. |
| Determinism / headless | The whole runtime (update → view → reconcile → effects) runs without a device; effects are values the harness records. |

So the thesis is: **Elm's semantics at the retained tree's cost.** The view is
rebuilt as a cheap description; the widget tree is *reconciled*, not rebuilt.

## 2. The program

### 2.1 The trait

```rust
pub trait Program: 'static {
    type Model: 'static;
    /// Widgets emit these; `Send` so commands and subscriptions can deliver
    /// them from other threads (the same bound `App::Message` has).
    type Msg: Clone + Send + 'static;
    /// Startup input (CLI args, config) — Elm's `flags`.
    type Flags;

    fn init(flags: Self::Flags) -> (Self::Model, Cmd<Self::Msg>);
    fn update(model: &mut Self::Model, msg: Self::Msg) -> Cmd<Self::Msg>;
    fn view(model: &Self::Model) -> Node<Self::Msg>;

    fn subscriptions(_model: &Self::Model) -> Sub<Self::Msg> { Sub::none() }
    /// The theme is a function of the model too (a settings toggle switches
    /// it); the runtime calls `Ui::set_theme` only when it changes.
    fn theme(_model: &Self::Model) -> Theme { Theme::dark() }
    fn fonts() -> Vec<Vec<u8>> { Vec::new() }
    fn idle_policy(_model: &Self::Model) -> IdlePolicy<Self::Msg> { IdlePolicy::disabled() }
}
```

`update` takes `&mut Model` rather than returning a new model: it is the
idiomatic Rust rendering of the same contract (iced, relm4 do the same), avoids
cloning large models, and costs no purity that matters — the runtime still
owns the model and `update` still cannot touch the tree. There is no `Ui` in
scope anywhere in a program; that is the point.

### 2.2 The counter

```rust
struct Counter;
#[derive(Clone)] enum Msg { Inc, Dec }

impl Program for Counter {
    type Model = i32; type Msg = Msg; type Flags = ();
    fn init(_: ()) -> (i32, Cmd<Msg>) { (0, Cmd::none()) }
    fn update(n: &mut i32, msg: Msg) -> Cmd<Msg> {
        match msg { Msg::Inc => *n += 1, Msg::Dec => *n -= 1 }
        Cmd::none()
    }
    fn view(n: &i32) -> Node<Msg> {
        column().fill().padding(24.0).gap(16.0).align(Align::Center).children([
            label("Counter").size(28.0).bold(),
            label(n.to_string()).size(48.0),
            row().gap(12.0).children([
                button("−", Msg::Dec),
                button("+", Msg::Inc),
            ]),
        ])
    }
}

fn main() -> fbui::Result<()> { fbui::run_program::<Counter>(()) }
```

Compared with `fbui/examples/counter.rs`: no `label: Option<WidgetId>`, no
`ui.with`, and the label cannot go stale. On `Msg::Inc` the reconciler finds
one `Label` whose text differs, calls `Ui::with(set_text)`, and the damage is
that label's rect — identical to what the hand-written version produces.

### 2.3 Relationship to `App`

`App` is not deprecated. `fbui::run_program::<P>(flags)` is implemented as
`fbui::run(Runtime::<P>::new(flags))` where `Runtime<P>: App` — the runtime
*is* an `App` whose `build` runs `init` + first reconcile, whose `update` feeds
the program, and whose `on_start` keeps the `Proxy` for effects. Nothing in
the runner changes for this; the runner keeps owning the frame clock, gestures,
cursor, hotplug, power and record/replay. The layering becomes:

```
fbui           umbrella: run / run_program, re-exports elm
fbui-elm       Program, Node/Element, reconciler, Cmd/Sub, Harness   [this doc]
fbui-widgets   retained tree, focus, theming, gestures, animation    [DESIGN.md]
fbui-render    …
```

`fbui-elm` depends on `fbui-widgets` only and is headless; it must not know
about `fbui-platform`. The effect *executor* that needs `Proxy` lives in the
`fbui` runner behind `platform`, exactly where `Proxy` already lives. The
headless executor (the test harness) lives in `fbui-elm`.

## 3. The view tree

### 3.1 `Node` and `Element`

`view` returns a `Node<Msg>`: a lightweight description of a widget subtree.
It is *not* a widget — building it allocates a few small structs and strings,
never a `TaffyTree` node, never a glyph. It is consumed by the reconciler.

```rust
pub type Node<Msg> = Box<dyn Element<Msg>>;

/// A description of one retained widget plus its children.
pub trait Element<Msg>: 'static {
    /// Identity of the widget type this element produces. Two elements with
    /// different kinds never reconcile into each other.
    fn kind(&self) -> Kind;               // newtype over TypeId (+ mapper chain, §3.5)
    /// Optional explicit key for keyed diffing (§4.3).
    fn key(&self) -> Option<&Key>;
    /// First render: build the retained widget and hand back the children
    /// to recurse into.
    fn create(self: Box<Self>) -> Built<Msg>;
    /// Re-render: push this element's props into an existing widget of the
    /// same kind, reporting what changed and the children to recurse into.
    fn patch(self: Box<Self>, widget: &mut dyn Any) -> Patched<Msg>;
}

pub struct Built<Msg>   { pub widget: Box<dyn Widget<Msg>>, pub children: Vec<Node<Msg>>, pub attrs: Attrs }
pub struct Patched<Msg> { pub change: Change, pub children: Children<Msg>, pub attrs: Attrs }

pub enum Children<Msg> { Diff(Vec<Node<Msg>>), Skip /* lazy hit, §3.4 */ }
pub enum Change { None, Paint, Layout }   // = what the Ui must be told
pub struct Attrs { tooltip: Option<Tooltip>, autofocus: bool }   // Ui-level facilities, §4.6
```

Elements are consumed by value (`self: Box<Self>`) so their props *move* into
the widget — a 10k-row `Vec<String>` for a `List` is moved, not cloned, and
`patch` can hand the whole vector to `List::set_rows` when it differs.

### 3.2 Built-in elements and the `adopt` contract

Every built-in widget gets an element with the same builder vocabulary the
widget already has, so `label("x").size(28.0).bold()` reads like
`Label::new("x").size(28.0).bold()`. The mapping is mechanical because of one
new method on each widget:

```rust
impl Label {
    /// Take the *configuration* of a freshly built `fresh`, keep this widget's
    /// *state*, and say what changed. The reconciler's whole contract.
    pub fn adopt(&mut self, fresh: Label) -> Change { … }
}
```

For `Label`: text/size/bold/wrap differing → `Change::Layout` (the measure
changes); color only → `Change::Paint`; nothing → `Change::None`. For
`TextInput`: placeholder → `Paint`; `value` differing from the *current* text →
set it and clamp the caret → `Paint`; `on_change` callback → always re-adopted,
`Change::None` (callbacks are not comparable and do not affect pixels). For
`Container`: gap/padding/align/grow/size → `Layout`; background → `Paint`.

The rule that decides every case: **a prop is compared, a callback is
replaced, state is kept.** `adopt` is the one place per widget where "what is a
prop and what is state" is written down, which is why it belongs in
`fbui-widgets` next to the widget rather than in the Elm layer. Each widget's
element `patch` is then three lines: downcast, `adopt`, return children.

Widget state that survives a render, by construction: `ScrollView` offset and
kinetic velocity, `TextInput`/`TextArea` caret and selection and scroll,
`Button` hover/pressed visuals, `Slider` drag, every `Tween`/`Spring`, `Select`
open/closed, `Navigator` transition, `Toasts` queue and TTLs, `Spinner` phase.

### 3.3 Keys

```rust
column().children(
    model.todos.iter().map(|t| keyed(t.id, todo_row(t)))
)
```

A `Key` is `u64 | &'static str | String` (an enum, hashed). Keys are scoped to
their parent's child list. Without keys, children match by index and kind;
with keys, by key and kind (§4.3). Keys are what let a removed row take its
own widget with it instead of shifting every sibling's state up by one.

### 3.4 `lazy`

```rust
lazy(("chart", model.samples.len()), |_| big_chart(&model.samples))
```

Elm's `Html.Lazy`. `lazy(args, f)` stores `args: impl PartialEq + Clone +
'static` in the retained node's memo; on re-render, if the memo equals the new
args the element returns `Children::Skip` with `Change::None` and the whole
subtree is neither built nor visited. This is the tool for the 10k-row screen
and for anything expensive to describe. It is also how a subtree can opt out
of re-rendering during an unrelated high-rate update (a clock ticking every
second must not cause a 500-node diff of the page beside it — `lazy` the
page, or the diff *is* cheap enough; §6.3 has the numbers to decide by).

### 3.5 Composing programs: a mapper argument, not `Html.map`

```rust
fn view(m: &Model) -> Node<Msg> {
    column().children([
        settings::view(&m.settings, Msg::Settings),
        player::view(&m.player, Msg::Player),
    ])
}

// settings.rs — a child module's view is a function taking the wrapper
pub fn view<M: Clone + 'static>(m: &Model, wrap: impl Fn(Msg) -> M + Clone + 'static) -> Node<M> {
    column().children([
        switch("Dark theme", m.dark, { let w = wrap.clone(); move |on| w(Msg::Dark(on)) }),
        button("Reset", wrap(Msg::Reset)),
    ])
}
```

Composition follows Elm's stance — components are functions, not objects with
private state; the model is the only state — but v1 does **not** provide
Elm's `Html.map` (`Node<A> -> Node<B>`). The retained tree stores `Widget<B>`
for one concrete `B`, so a real `map` has to reach the widget level: a
`Mapped<A, B>` wrapper widget forwarding every `Widget` method, a
`EventCtx::remap` / `AnimCtx::remap` pair in `fbui-widgets/src/ctx.rs` to hand
the inner widget a sibling context and translate what it emits, a composite
`Kind`, and a lazily-wrapped child walk. It is buildable (`Outputs` is
crate-private, so `remap` is the one place that can construct the sibling
context, ~30 lines) but it is the most invasive prerequisite in the design,
it muddies `Ui::inspect` names and islands inside mapped subtrees, and it is
the least necessary: a child module's `view` taking `wrap: impl Fn(Child) ->
Parent` gives the same modularity with zero new machinery, because callbacks
are constructed at element-build time anyway. Child `update`s compose the
same way (`settings::update(&mut m.settings, msg).map(Msg::Settings)` on the
`Cmd`, which *is* cheap to map since it is data).

`Node::map` stays on the table as a post-v1 addition once the mapper-argument
pattern has been used in anger; its design above is recorded so it is not
re-derived.

### 3.6 Islands: the escape hatch

Some widgets are driven at rates or through APIs a description cannot express:
a `Chart` fed samples at wire rate via `Ui::stream`, a `VideoView` given
frames, a third-party `Widget` with no element yet. For these:

```rust
island(key, || Chart::new(…), |chart: &mut Chart, ui_ops| { … })
```

`island(key, create, drive)` creates the widget once (by key + type) and on
every render calls `drive` with `&mut W` and an `IslandOps` that exposes
`stream`-style damage reporting (`Quiet` / `Repaint` / `Shifted`). The
closure captures whatever model slice it needs. This is deliberately a
per-widget hole, not a general "reach into the `Ui`" API: islands cannot add
children or move focus, so the tree stays the reconciler's. High-rate feeds
that must bypass `view` altogether (telemetry at kHz) do so with
`Cmd::stream(key, |w: &mut Chart| …)` (§5.1), which runs before the next
paint without re-rendering.

## 4. Reconciliation

### 4.1 The shadow

The reconciler keeps, per retained widget, a small record: the `Kind`, the
`Key`, the lazy memo, the attrs last applied, and the children in order. This
*shadow tree* is indexed by `WidgetId` (a `SecondaryMap`) and is the only
thing the diff reads — the reconciler never asks the `Ui` about structure
except `child_ids` in debug assertions. It never keeps the previous `Node`
tree: props live in the widgets, which is what makes "the previous view" free.

### 4.2 The walk

```
reconcile(parent_id, slot: Option<WidgetId>, node: Node<Msg>):
  match slot with same kind (and key, if either side has one):
    Some(id) →
      Patched { change, children, attrs } =
          ui.patch(id, |widget: &mut dyn Any| node.patch(widget))
      (Ui::patch reads `change` — None: touch nothing; Paint: damage the
       widget's rect; Layout: damage + re-apply style + relayout)
      apply attrs delta (tooltip / autofocus) if it changed
      if children is Diff(kids): reconcile_children(id, kids)
    None →
      Built { widget, children, attrs } = node.create()
      id = ui.insert_child(parent, index, widget)      (new Ui API, §8)
      apply attrs; reconcile_children(id, children)   (all creates)
```

`Change::None` calls **nothing** on the `Ui`. That is the property §1.3 asks
for: a message whose `update` changes nothing visible costs one `view`, one
walk, and zero damage; the runner then sees `needs_paint() == false` and stays
in `poll`.

`Ui::patch` is a new, small `Ui` method (§8) and not a reuse of `with` or
`stream`, for a mechanical reason: the reconciler only gets `&mut W` *inside*
a `Ui` closure, yet which damage the `Ui` should record is only known once
`patch` returns. `with` always damages and relayouts; `stream` never
relayouts. Neither can be chosen before the closure runs, so the closure
must return the verdict:

```rust
impl<Msg: 'static> Ui<Msg> {
    /// Mutate a widget, letting the closure say what the mutation changed.
    /// `Change::None` records nothing; `Paint` damages the widget's rect;
    /// `Layout` additionally re-applies its layout style and schedules a
    /// relayout (what `with` does unconditionally).
    pub fn patch<R>(&mut self, id: WidgetId, f: impl FnOnce(&mut dyn Any) -> (Change, R)) -> Option<R>;
}
```

`with` becomes `patch` with a constant `Layout` verdict, and `stream` keeps its
own richer `StreamDamage` for the blit case; the three share the lookup and
the damage bookkeeping. Skipping the relayout on `Paint` matters because a
relayout is cheap when nothing moved but not free, and the widget has just
said its geometry is unchanged. `Layout` re-applies the layout style (the very
thing that changed) and damages the old rect; the layout pass damages the new
one.

### 4.3 Children: keyed diff

For each parent, old children (from the shadow, in order) and new children
(the `Vec<Node>`):

1. **Match.** A new child claims an old child by `(key, kind)` when it has a
   key, else by `(index among unkeyed siblings, kind)`. Each old child is
   claimed at most once.
2. **Remove** every unclaimed old child: `ui.remove(id)` (subtree, focus and
   capture cleanup are the `Ui`'s existing behavior).
3. **Patch or create** in new order: claimed → `reconcile` into it; unclaimed
   → `create` + `insert_child` at the target index.
4. **Reorder.** If the claimed children are not already in the target order,
   `ui.move_child(parent, id, index)` per displaced node — a minimal set of
   moves computed by longest-increasing-subsequence over old indices, the
   standard virtual-DOM approach. Moves damage the union of old and new rects
   via the layout pass, which is correct and no worse than today's
   `remove`+`add_child`.

Complexity is O(n) per child list with a small hash map for keyed lists;
unkeyed lists need no map at all. Mixed keyed/unkeyed siblings are allowed
but a debug assertion flags duplicate keys.

### 4.4 Identity and state loss, stated

A retained widget lives as long as, on every render, its parent still has a
child at the same (key or index) with the same kind. Anything else — a kind
change, a key change, a parent change, an unkeyed insertion before it —
destroys it and creates a new one, losing its state. This is the same rule as
React's and Elm's, and the same footgun: an unkeyed list of text inputs where
a row is inserted at the top will shift every caret. The fix is keys; the
docs say so in the first paragraph about lists.

`Dialog` inherits this cleanly: `if model.confirm { dialog(…) }` as the last
child of a `stack` appears and disappears as structure, focus moves in via
`.autofocus()` (§4.6), Esc/scrim emit `Msg::CloseDialog`, and the reconciler
removes the subtree — the exact sequence `widgets/dialog.rs` documents, with
the app no longer holding the id.

### 4.5 Widgets that own child lifecycle

Two widgets today mutate the tree through static helpers because they animate
structure: `Navigator::push/pop/settle` and `Toasts` (paint-only, no children).
`Navigator` is the one that needs reconciler cooperation:

```rust
navigator(model.screens.iter().map(|s| keyed(s.id, screen(s))))
```

Its element's children are the screen stack. The reconciler's generic diff
would `add_child`/`remove` screens instantly, skipping the slide; instead the
`NavigatorElement` overrides child reconciliation: a new keyed top screen →
`Navigator::push`; a removed top screen → `Navigator::pop` (the widget reaps
the node after the slide via `take_child_removals`, which the `Ui` already
polls); a middle-of-stack change → `settle` then generic diff. The shadow
records screens the widget has *promised* to remove so the next render does
not see them as "new". This is the only element in v1 with custom child
reconciliation; the hook is `Element::reconcile_children` with a default that
runs §4.3, and it exists so that a downstream structural widget can do the
same.

### 4.6 Attributes that are `Ui` facilities

Two things a view wants to say are not widget props but `Ui` calls:

* `.tooltip("text")` → `Ui::set_tooltip` / `clear_tooltip`, applied when the
  attr differs from the shadow's copy.
* `.autofocus()` → on *create* only, `Ui::focus_first(id)` after the subtree
  exists (a dialog opening, a screen pushed). Re-renders never steal focus;
  moving focus later is a `Cmd::focus(key)` (§5.1), Elm's `Dom.focus`.

Named focus targets: `Cmd::focus(key)` resolves a key through a
reconciler-maintained `Key → WidgetId` index of *keyed* nodes (keys become
globally addressable by their path of keys: `"settings" / "name"`). A key that
resolves to nothing is a no-op with a `debug_assert`, like Elm's
`Dom.NotFound`.

## 5. Effects

### 5.1 `Cmd`

```rust
pub enum Cmd<Msg> {
    None,
    Batch(Vec<Cmd<Msg>>),
    /// Dispatch immediately, after the current update (Elm's "just send").
    Msg(Msg),
    /// Deliver once after a delay — `Proxy::send_after`.
    After(Duration, Msg),
    /// Run on a worker thread, deliver the result — `Proxy::send` from a
    /// `std::thread::spawn`. `Perform(Box<dyn FnOnce() -> Msg + Send>)`.
    Perform(…),
    /// Ui facilities, resolved by key (§4.6).
    Focus(KeyPath), Blur, ScrollTo(KeyPath, ScrollTarget),
    SetClipboard(String), Screenshot(PathBuf),
    /// Drive one island widget before the next paint, without a render (§3.6).
    Stream(KeyPath, Box<dyn FnOnce(&mut dyn Any) -> StreamDamage>),
    Quit,
}
```

`Cmd` is plain data with constructors (`Cmd::after(d, msg)`, `Cmd::perform(f)`,
`Cmd::batch([...])`) and a `map(f: Fn(A) -> B)` that wraps the payload of
`Msg`/`After`, composes onto `Perform`'s closure, and passes the rest through;
`Sub` has the same. This is what lets a child module's `update` return
`Cmd<child::Msg>` and the parent lift it (§3.5). The runtime executes commands *after* reconciling the
render that follows the update that produced them, so a `Focus` sees the tree
it targets. `Perform` is threads, not async: this framework has no executor,
`Proxy` is `Send + Clone`, and a kiosk's background work is an IPC reader or a
file read, both fine on a thread. An `async` adapter can be layered later
without changing `Cmd`.

### 5.2 `Sub`

```rust
pub enum Sub<Msg> {
    None,
    Batch(Vec<Sub<Msg>>),
    /// A repeating timer — `Proxy::send_every`. Identity: `(period, id)`.
    Every { period: Duration, msg: Msg, id: SubId },
    /// A long-lived worker thread that feeds messages until unsubscribed.
    Worker { id: SubId, start: Rc<dyn Fn(Feed<Msg>) -> Stop> },
    /// Runner events the program may care about.
    OnSession(fn(bool) -> Msg), OnDisplayChanged(fn(DisplaySize) -> Msg),
}
```

Elm's subscriptions are declarative: `subscriptions(model)` is called after
every update and the runtime **diffs** the result against the running set —
new entries are started, missing ones cancelled, unchanged ones untouched. A
clock that should tick only while a screen is visible is `if model.screen ==
Clock { Sub::every(1s, Msg::Tick) } else { Sub::none() }`, and the `Timer`
handle is cancelled for the program by the diff. This replaces the
`ticker: Option<Timer>` field plus manual `cancel()` in `examples/timer.rs`.

Identity is the subtle part: entries must be matched across renders without
the payload participating (`Msg::Tick(n)` must not restart the timer every
second). `SubId` defaults to `(discriminant(&msg), period)` for `Every` —
`std::mem::discriminant` is `Hash + Eq` and ignores payload — and
`.with_id("name")` overrides it when two subscriptions would collide. A
`Worker` always names its id. The runtime holds `HashMap<SubId, Running>`
where `Running` is a `Timer` or a `Stop` handle; dropping/cancelling is the
whole unsubscribe.

Ignoring the payload for *identity* means the payload can change while the
subscription stays running: `Sub::every(1s, Msg::Tick(model.generation))`
matches the existing timer on every render, and the queued message must then
be the new one, not the one captured when the timer started. So the diff has
three outcomes per entry, not two: start, cancel, or **update the payload in
place**. `TimerQueue` has no such operation today; `Timer::replace(msg)` (a
lock, swap the stored message, keep the deadline and period) is the small
addition listed in §8. Cancel-and-restart would work but resets the phase,
which turns a once-per-second clock into a stutter whenever the payload
changes. `Worker` payloads live in the closure and are not updated; a worker
that needs fresh model state should receive it through a message from
`update`, not by re-subscribing.

`Worker` gets a `Feed<Msg>` (a thin wrapper over `Proxy` that also exposes
`is_stopped()`), so an IPC reader loop is `while !feed.is_stopped() {
feed.send(Msg::Line(read()?)); }`. The `Stop` it returns is what the diff
calls; the worker thread notices on its next send or poll.

### 5.3 Headless execution

In the harness (§7.1) effects are **recorded, not run**: `Cmd::After` and
`Cmd::Perform` land in `harness.pending()` as data (`Effect::After(d, msg)`,
`Effect::Perform(id)`), and the test chooses when to fire them
(`harness.fire_after()` advances the deterministic clock, `harness.run_perform(id)`
runs the closure inline on the test thread). `Sub` diffs are visible as
`harness.subscriptions()`. `Focus`/`Screenshot`/`Stream` run against the
headless `Ui` immediately since they need no platform. This is what makes a
program's effect logic unit-testable without threads or time.

## 6. The runtime loop

### 6.1 One frame

```
messages ← Ui::take_messages() ++ Proxy/timer inbox   (all of them)
for msg in messages:  cmds += P::update(&mut model, msg)
if any message ran:
    node = P::view(&model)               ← once per frame, not per message
    reconciler.reconcile(&mut ui, node)  ← damage only where props differ
    theme / idle policy re-applied if changed
    subs.diff(P::subscriptions(&model))
    execute(cmds)                        ← after the tree they target exists
(runner: layout, paint damaged, present — unchanged)
```

Coalescing is deliberate: a burst of ten pointer moves that each produce a
`Msg::Drag` runs `update` ten times but `view` once. Elm does the same
(renders on animation frame). It also bounds the cost of high-rate feeds to
one diff per frame regardless of message rate.

### 6.2 Where the retained layer still does the work

Everything time-driven stays inside widgets and the `Ui`: kinetic scroll,
tweens, `Spinner`, tooltip dwell, `Navigator` slides, `Toasts` TTLs all run in
`Ui::animate` on the frame `dt`, with no message traffic and no `view` call.
Hover, press visuals and focus rings likewise never reach the program. Only
*semantic* events become messages — the same set that reaches `App::update`
today. This is why the layer does not regress animation cost: an animating
screen re-renders zero times per frame unless a message arrives.

### 6.3 Cost model and the numbers to measure

Per rendering message: allocate the `Node` tree (a few hundred small boxes and
strings for a typical kiosk screen) + one walk with a `Kind` compare and a
prop compare per node + `adopt` moves. Estimated well under a millisecond for
~500 nodes on a Pi 4 class core; the reconciler benchmark (§7.4) turns the
estimate into a gate. The two escape valves if a screen is bigger are `lazy`
(skip subtrees whose inputs are unchanged) and keeping high-rate data out of
`view` (`Cmd::stream` / islands). Neither is needed for the counter, the
form, or the showcase.

## 7. Testing

### 7.1 `Harness<P>`

```rust
let mut h = Harness::<TodoApp>::new((), Size::new(480.0, 320.0));
h.type_text("new-todo", "buy milk");
h.click("add");
assert_eq!(h.model().todos.len(), 1);
assert!(h.find("todo-1").is_some());          // key → bounds
h.key(Key::Escape);
assert_eq!(h.pending(), &[Effect::After(Duration::from_secs(3), Msg::HideToast)]);
h.fire_after();                                // deterministic clock
h.snapshot("todo_after_add");                  // fbui-testkit golden
```

The harness owns a headless `Ui`, the model, the reconciler and the recorded
effects. It drives input through `Ui::event` — the real event path, the same
one the runner uses — and resolves keys to bounds for `click`/`type_text`
(the on-screen `Keyboard` pattern: replay keys through `Ui::send_key`).
`tick(dt)` runs `Ui::animate` so tests can settle transitions.

### 7.2 Equivalence invariants (the ones that gate merging)

1. **`reconcile_matches_fresh_build`** — for a sequence of models `m0..mn`,
   the pixels of a `Ui` reconciled through `view(m0)…view(mn)` equal the
   pixels of a fresh `Ui` built from `view(mn)` alone (same size/scale/theme,
   after `layout_now`+`paint`). Run over the examples' scenarios and a seeded
   random walk of a todo model. This is the scroll-blit rule applied to the
   reconciler: the incremental path may never diverge from the full one.
2. **`unchanged_render_produces_no_damage`** — reconcile `view(m)` twice;
   after the second, `ui.needs_paint()` is `false` and no `Ui` mutation was
   called (a counting shim over the reconciler's `Ui` calls).
3. **`state_survives_rerender`** — scroll a `ScrollView`, place a caret in a
   `TextInput`, start a tween; re-render with a changed unrelated label; all
   three are intact. Then re-render with the input's *value* changed by the
   model; the text updates and the caret clamps rather than resets.
4. **`keyed_rows_keep_their_widgets`** — insert a row at index 0 of a keyed
   list; every existing row's `WidgetId` is unchanged (shadow inspection via
   `Ui::inspect`), and the unkeyed variant of the same test shows the shift
   (so the doc's warning is pinned, not folklore).
5. **`child_view_wraps_messages`** — a child module's button, built with
   `wrap = Msg::Child`, emits `Msg::Child(child::Msg::Pressed)` through the
   real `Ui::event` path, and the child's `Cmd` mapped with `Cmd::map` arrives
   wrapped too.
6. **`subscription_payload_updates_without_restart`** — re-render with
   `Sub::every(1s, Msg::Tick(n+1))`; the timer's deadline is unchanged and
   the next delivery carries `n+1`.

### 7.3 Snapshot parity with the retained examples

`counter`, `form`, and `showcase` get Elm twins; a test builds each pair
headless at the same size and theme and asserts pixel equality through the
`fbui-testkit` compare (one golden per pair). Two ways of describing the same
screen must paint the same pixels, and the builder vocabulary drift, if any,
shows up here.

### 7.4 Benchmark gate

`cargo bench -p fbui-elm --bench reconcile`: a 1,000-node screen (keyed list
of rows with a label, a checkbox, a button each). Cases: no-change render,
one-label change, one-row insert at 0 (keyed), full replace. Gate: no-change
and one-label ≤ 2× the cost of building the `Node` tree alone (i.e. the walk
is not the expensive part), and a full replace ≤ 1.5× a fresh `set_root`
build. Numbers go in `PHASE7-elm.md` when the phase closes, like Phase 5's.

## 8. Changes required in existing crates

All additive; none changes a public behavior an `App` relies on.

`fbui-widgets`:

| Change | Why |
|---|---|
| `fn adopt(&mut self, fresh: Self) -> Change` on every widget in `widgets/` (and `Change` in `widget.rs`) | The prop/state split, §3.2. Also useful to retained apps: `ui.with(id, |l| l.adopt(Label::new(..).bold()))`. |
| `Ui::insert_child(parent, index, widget)` and `Ui::move_child(parent, id, index)` | Keyed diff needs insert-at and reorder; today only `add_child` (append) exists. Both are `taffy` `insert_child_at_index` / `remove_child` + `insert` and a `Vec` edit on `Node::children`, then `mark_full`. |
| `Ui::patch(id, \|w\| -> (Change, R))` with `with` reimplemented over it | The reconciler's verdict-driven damage, §4.2. |
| `Ui::child_index(id) -> Option<usize>` (or expose in `InspectNode`) | Reconciler debug assertions and the harness's key index. |
| Debug: `Ui::mutation_count()` behind `cfg(test)`/feature | Invariant 7.2.2 counts mutations rather than inferring from damage. |

`fbui` (umbrella, `platform`):

| Change | Why |
|---|---|
| `Runtime<P>: App` and `pub fn run_program<P: Program>(flags: P::Flags) -> Result<()>` | §2.3. |
| Effect executor over `Proxy`/`Timer`: `After`, `Perform`, `Every`, `Worker`, `OnSession`, `OnDisplayChanged` | §5. `on_session` and `on_display_changed` are runner callbacks today; they get forwarded as messages when subscribed. |
| `Timer::replace(msg)` on the timer queue | Subscription payload updates without restarting the timer, §5.2. |
| Examples: `elm_counter`, `elm_form`, `elm_todo` (keyed list + dialog + navigator + toast + subscription) | Parity tests and the docs' worked examples. |

New crate `fbui-elm` (MSRV 1.89, tracks the widget stack; `publish = false`
until the API settles): `program.rs`, `node.rs` (`Element`, `Key`, `lazy`,
`island`), `el/` (one file per built-in element), `reconcile.rs`,
`cmd.rs`, `sub.rs`, `harness.rs` (behind a `test-support` feature or as
`fbui-elm::harness`, dev-dependency on `fbui-testkit`).

## 9. Non-goals and open questions

**Non-goals for v1.**

* A `view!` macro DSL. Builders compose fine in Rust and are what the widgets
  already speak; a macro can be pure sugar over `Node` later (this is also
  what PLAN §Phase 7 literally lists, and this design is its foundation).
* An async runtime. Threads + `Proxy` cover the kiosk case; `Cmd::Perform` can
  grow an `async` variant behind a feature without changing the shape.
* Component-local state. A child `view` taking a mapper is the composition
  tool; anything with private state that is not model state is a *widget*
  (retained) or an island. This is Elm's position and it keeps "the model is
  the screen" true.
* `Node::map` (Elm's `Html.map`). Deferred, with its design recorded in §3.5;
  the mapper-argument pattern covers v1.
* Replacing `App`. Both stay; the runtime is an `App`.
* Elm's `Html.Keyed` as a separate node type: keys are an attribute of any
  child (`keyed(k, node)`), which is simpler and covers the same cases.

**Open questions, with the current lean.**

1. *`update(&mut Model)` vs `update(Model) -> Model`.* Lean: `&mut`, §2.1.
   Reconsider only if a time-travel debugger for the remote console wants
   cheap model snapshots; a `Model: Clone` bound on that feature would do.
2. *Should `Change::Paint` bypass relayout?* Lean: yes via `Ui::patch`, §4.2.
   Risk: a widget whose `adopt` under-reports (`Paint` when the measure
   changed) paints stale geometry. Mitigation: invariant 7.2.1 catches it, and
   `adopt` implementations default to `Layout` when unsure — the same
   "when unsure, request_paint" rule as today, one level up.
3. *Unkeyed containers of dynamic length.* Lean: allowed, index-matched, with
   the state-shift footgun documented and pinned by test 7.2.4. Alternative
   is to require keys for any list built from an iterator; too strict for
   static layouts (`children([a, b, c])`).
4. *Where does the theme come from?* Lean: `Program::theme(&Model)` so a
   settings toggle is just a model change (`Ui::set_theme` damages the root,
   once). Alternative: `Cmd::SetTheme`; rejected because the theme is state.
5. *Islands versus first-class elements for `Chart`/`Gauge`/`VideoView`.*
   Lean: elements for their configuration (ranges, colors), `Cmd::stream` for
   their data, so a telemetry screen is still fully declarative except the
   sample feed. `VideoView` frames are the same shape.

## 10. Plan

Sequenced so each step lands green on its own and the equivalence tests exist
before anything depends on them.

**Step A — widget-crate prerequisites** (`fbui-widgets`): `Change` + `adopt`
on every widget, `Ui::patch` (with `with` re-expressed over it),
`insert_child`/`move_child`, `Timer::replace`.
*Exit:* `cargo test --workspace` green; an `adopt` round-trip test per widget
(`adopt(fresh)` then snapshot equals a widget built as `fresh`); a
`patch`-with-`Change::None` test asserting `needs_paint()` stays false.

**Step B — `fbui-elm` headless**: `Program`, `Node`/`Element`, built-in
elements, keyed reconciler, `lazy`, `island`, `Cmd`/`Sub` types (with
`Cmd::map`), headless `Runtime` and `Harness`. *Exit:* invariants 7.2.1–6
pass; `Harness`
drives an Elm counter and form to the retained goldens (7.3); bench compiles
with a first number recorded.

**Step C — runner integration** (`fbui`, `platform`): `run_program`, effect
executor over `Proxy`/`Timer`, `Sub` diffing, session/display forwarding,
`Navigator` element with push/pop cooperation, `elm_*` examples. *Exit:*
examples run on VKMS/terminal backend; `FBUI_HUD=1` shows 0 frames/s idle on
the Elm counter; the record/replay of a retained example replays identically
against its Elm twin; bench gate (7.4) enforced in CI's bench-compile job.

**Step D — docs and closure**: `docs/elm.md` (tutorial from counter to todo),
`CHANGELOG.md`, `PHASE7-elm.md` with verified-vs-pending exit criteria in the
style of `PHASE5.md`, cross-links from `fbui-widgets/DESIGN.md §8` and
`CLAUDE.md`. *Exit:* `RUSTDOCFLAGS="-D warnings" cargo doc` clean with
`fbui-elm` included; the umbrella lib doc's leading example is the Elm
counter with the retained one immediately after.

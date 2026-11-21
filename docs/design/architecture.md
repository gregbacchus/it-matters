# Architecture

How it-matters is put together, and in particular the two **seams** that keep the parts independent: the
**plugin protocol** (between plugins and the host) and the **frontend channel boundary** (between the host and
the Matter side). Read this before changing either. The wire contract for plugins is in
[`plugin-protocol.md`](plugin-protocol.md); what controllers see is in [`../matter.md`](../matter.md).

## The picture

```text
 plugin processes                     the it-matters daemon                              Matter controllers
 (any language)                                                                          (Apple, Google, HA)
┌──────────────┐  Unix socket   ┌──────────────────────────────────┐   ┌──────────────────────────────┐
│ linux-system │ ─────────────▶ │  host (tokio, main thread)       │   │ Matter side (own thread)     │
│ linux-audio  │  JSON lines    │   sessions ─▶ actor ─▶ registry  │   │  rs-matter stack + handlers  │
│ your plugin  │                │        ▲                 │       │   │                              │
└──────────────┘                │        │ supervisor      ▼       │   │   one handler per cluster    │
┌──────────────┐  TCP + proof   │        │ (starts/        events ═══════▶ pump ─▶ endpoint table     │ ◀─▶ Matter
│ remote agent │ ─────────────▶ │        │  restarts)      ▲       │   │            ▲                 │     (UDP 5540,
│ (other host) │                │                          commands ◀════ controller writes            │      mDNS)
└──────────────┘                └──────────────────────────────────┘   └──────────────────────────────┘
                                            ══▶ = the frontend channel boundary
```

## Crates

| Crate | Role | Knows about |
|---|---|---|
| `it-matters-model` | The Matter-agnostic **device and capability model**, the **wire messages** and their framing, and the **authentication proof**. No IO, no Matter. | serde, HMAC |
| `it-matters-core` | The **host**: mapping, endpoint ids, registry, sessions, supervisor, and the frontend channel types. | tokio; **not** rs-matter |
| `it-matters-matter` | The **Matter side** on rs-matter: endpoint table, handlers, commissioning codes, the stack. | rs-matter; the channel types from core |
| `it-matters` | The daemon binary: configuration, tokens, wiring the two threads. | all of the above |
| `it-matters-plugin-sdk`, `plugins/*` | Rust plugin client and the bundled plugins. | the model only (tests also use core) |

Because core never imports rs-matter and the Matter crate never imports tokio sockets, the whole host is tested
with a fake Matter side, and the Matter side with its table logic unit-tested separately.

## Seam 1: the plugin protocol

Everything a plugin may do is in [`plugin-protocol.md`](plugin-protocol.md). In the code:

* `it-matters-model::protocol`: the message types and `encode_line` / `decode_line` (line cap 64 KiB).
* `it-matters-core::host`: sessions (one task per connection: framing, handshake, heartbeat) and the **actor**
  (one task that owns all cross-plugin state, so changes happen in one total order and no lock is held across
  an await). Sessions are generic over the stream, so the Unix socket, TCP and in-memory pipes in tests share
  the code. `Access` says how a connection is authenticated and which plugin it may be.
* `it-matters-core::mapping`: capabilities → units (device kinds), level scaling, mute polarity.
* `it-matters-core::registry`: the **lifecycle state machine** (ownership, reachability, re-registration,
  routing of controller writes). Pure and synchronous: every operation returns the events to send, so it is
  tested exhaustively without sockets.
* `it-matters-core::allocator`: persisted endpoint ids, keyed by (device, unit, shape), never reused.
* `it-matters-core::supervisor`: starts, restarts and stops the plugins configured with `run`.

## Seam 2: the frontend channel boundary

`it-matters-core::frontend` defines two bounded `async-channel` channels, deliberately **runtime-agnostic**
because rs-matter runs under its own blocking executor on a separate thread while the host runs on tokio.

**Host → Matter side: `FrontendEvent`** (capacity 1024; a slow Matter side slows plugins down instead of
growing memory):

| Event | Meaning |
|---|---|
| `AddEndpoint(EndpointSpec)` | A bridged endpoint appears: id, `UnitKind`, display name, unique id, reachability, initial attribute values. |
| `RemoveEndpoint { endpoint }` | It disappears (its id stays reserved). |
| `SetAttr { endpoint, value }` | An attribute changed. `AttrValue` is already in Matter terms: `BooleanState(bool)`, `Occupancy(bool)`, `OnOff(bool)` (a Speaker's On = not muted), `CurrentLevel(u8)` (0-254). |
| `SetReachable { endpoint, reachable }` | The plugin connected or disconnected. |

**Matter side → host: `ControllerCommand`** (capacity 64): `{ endpoint, write, reply }` where `write` is
`Write::OnOff(bool)` or `Write::CurrentLevel(u8)` and `reply` is a one-shot channel the host answers with
`Ok(())` once the plugin applied the change, or `Err(reason)`. The Matter handler **waits for that answer**
before replying to the controller.

Rules the boundary guarantees: events arrive in the order the host decided them; an unchanged value is not
re-sent (the first report of a value always is); the host never sends an event for an endpoint it has not announced with `AddEndpoint`; the
host keeps serving plugins if the Matter side goes away (and the reverse).

`FakeFrontend` (in core) implements the Matter side for tests: it records events and lets a test play the
controller. A different frontend (another protocol) would implement the same two channels.

## The Matter side (`it-matters-matter`)

* `state::Table` holds every endpoint's state; `apply(event)` returns what changed. Pure and unit-tested.
* **One handler per cluster type serves every endpoint** (Boolean State, Occupancy Sensing, On/Off, Level
  Control, Bridged Device Basic Information). The endpoint id comes from each request and the data comes from
  the table, so the topology is dynamic while rs-matter's handler chain stays static.
* The **pump** runs inside a handler's `run()`: it receives `FrontendEvent`s, updates the table, rebuilds the
  node metadata when the topology changed, and calls rs-matter's notifiers so subscribers are told.
* Controller writes become `ControllerCommand`s (`shared.rs`).
* `run.rs` builds the stack: persistence under `matter.storage_dir`, mDNS through avahi, test attestation
  certificates, commissioning codes from `commissioning.rs`.

## Threads

| Thread | Runs | Talks to the other through |
|---|---|---|
| main (tokio, current-thread) | config, host actor, sessions, supervisor, signals | the frontend channels |
| `matter` | rs-matter (blocks in its own executor) | the frontend channels |

The supervisor must run on the main thread: it uses `PR_SET_PDEATHSIG`, which fires when the *thread* that
started a plugin exits.

## Persisted state (`matter.storage_dir`)

`matter/` (rs-matter: fabrics, commissioning), `endpoints.json` (endpoint ids), `commissioning.json` (generated
pairing codes, mode `0600`). Details: [`../config/matter.md`](../config/matter.md).

## Adding things

* **A new device kind** (a new Matter unit): `mapping.rs` (rule, shape, tests) → `frontend.rs` (`AttrValue` /
  `Write` if needed) → `state.rs` → `handlers.rs` + `node.rs` + the chain in `run.rs` → `registry.rs` (conversion
  and write routing) → docs (`plugin-protocol.md` section 4, `matter.md`, plugin pages).
* **A new protocol message**: `protocol.rs` (and a sample in its tests), `host.rs`, `plugin-protocol.md` (a
  heading for it: a test requires one), the SDK, and bump `proto` if existing plugins would break.
* **A different frontend**: implement the two channels of `frontend.rs`; nothing in the host changes.

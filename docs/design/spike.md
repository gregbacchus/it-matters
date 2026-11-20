# Phase 0 spike: rs-matter as the Matter foundation

Date: 2026-11-21. Verdict: **GO**. rs-matter can host the bridge, including runtime
add/remove of bridged endpoints. No fallback to a fixed endpoint set is needed.

## What was tested

rs-matter at git `4e4105a7e37d86a03ff2c9d0f6fc9b572bcce6c0` (workspace version 0.4.1, MSRV 1.92,
Apache-2.0), built on this machine (Ubuntu, avahi running) with
`cargo build -p rs-matter-examples --bin bridge --features avahi`. Controller: matter.js shell
(`@matter/nodejs-shell` 0.18.0, Apache-2.0), because `chip-tool` was not available.
The prototype changes to `examples/src/bin/bridge.rs` are in [`spike-bridge.patch`](spike-bridge.patch)
(apply to the rev above).

| Question | Result |
|---|---|
| Does the upstream bridge example build and run on Linux with avahi mDNS? | Yes, after one fix (below). |
| Does it commission over IP with the printed pairing code, no BLE? | Yes. Fabric and network settings are persisted. |
| Does the controller see Aggregator (ep1) + bridged endpoints with Bridged Device Basic Information? | Yes. |
| Can endpoints be added/removed **at runtime**? | **Yes.** |
| Do Contact Sensor and Boolean State work? | Yes: `stateValue` read returned `true`; device types `[Contact Sensor 0x15, Bridged Node 0x13]`. |
| Is `Reachable` readable on a bridged endpoint? | Yes (`true`; real logic is ours to supply). |
| Occupancy Sensing cluster available? | Generated (`occupancy_sensing` module exists); not exercised on the wire. |
| Speaker / Level Control / On-Off? | Device type and clusters exist; `speaker.rs` example upstream. Not exercised here. |

## Findings that shape the design

1. **Dynamic topology works through the `Metadata` trait.** `Node` is a borrowed slice, but
   `Metadata::access(|node| ...)` is closure-based, so a `Mutex<Vec<Endpoint<'static>>>` can back it.
   Adding/removing an endpoint changed the Aggregator's `PartsList` seen by the controller
   (`[2,3]` -> `[2,3,4]` -> `[2,3]`) with no restart. After any change call
   `ctx.notify_endpoint_changed(aggregator_id)` (and the new endpoint's id).
2. **`Node` invariants (from rs-matter docs):** endpoints sorted by strictly increasing id; an
   endpoint's cluster/attribute shape is fixed for its lifetime. So a device whose capabilities
   change must get a **new endpoint id**; ids are allocated from a persisted table, never reused.
3. **The handler chain is static; metadata is dynamic.** Handlers are matched by closures such as
   `|e, c| e == 4 && ...`. In the prototype handlers for ep4 exist permanently and metadata controls
   visibility. The real design needs a handler that dispatches by endpoint id into the registry
   (one custom `Handler` impl instead of per-endpoint chain links). This is the main Phase 3 work.
4. **Where to push state to Matter:** `ClusterHandler::run(ctx)` receives a `HandlerContext` that
   implements `notify_attr_changed` / `notify_endpoint_changed`. That is the hook for plugin state
   updates (wait on a channel in `run`, mutate state, notify).
5. **Upstream example bug at this rev:** the bridge example's 32 KiB bump arena
   (`BUMP_SIZE`) panics at start-up ("Out of bump memory") in a debug build. 64 KiB works (this is also
   what rs-matter's own driver tests use). Size it from the start; consider building release.
6. The upstream `BridgedHandler` `todo!()` stubs for `unique_id`/`handle_keep_active` are not reachable
   because the example only declares required attributes. We should still implement `unique_id`
   (stable per device id) and `node_label` so controllers can show/rename devices.
7. Crates.io availability of rs-matter was not verified. Plan on a **git dependency pinned to a rev**
   (and record it in `Cargo.lock`; note this affects `cargo publish`).
8. mDNS: the `avahi` feature worked (needs a running avahi-daemon). The `resolve` feature
   (systemd-resolved) is the alternative for hosts without avahi. Decide at packaging time.

## Test-tooling notes

- matter.js shell: `commission pair <node-id> --pairingCode <code> ` (Node >= 20; install scripts
  for BLE/USB natives are not needed). `nodes connect <id>`, then
  `attributes <cluster> read <attr> <node> <endpoint> --remote` (`--remote` bypasses the client cache).
  The controller caches endpoint structure from commissioning/connect, so an endpoint added later is not
  readable until a fresh commission or structure refresh. Real controllers (Home Assistant, Apple,
  Google) re-read the Descriptor on a parts-list change; **verify that in the interop checklist**.
  For CI, prefer `chip-tool` or a fresh commission per scenario.
- Build output must not go to `/tmp` here (tmpfs, ~15 GB, nearly full); use `CARGO_TARGET_DIR`
  on the main disk.

## Open items carried into the plan

- Prove subscription/notification delivery of the topology change with a real controller (Home
  Assistant in docker), since the matter.js shell did not refresh.
- Exercise Speaker (Level Control + On/Off) and Occupancy Sensing on the wire.
- Decide the endpoint-id allocation store (own table next to rs-matter's KV store).

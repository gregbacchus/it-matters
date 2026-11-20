# AGENTS.md: it-matters

it-matters is a **lightweight Rust Matter bridge that hosts plugins**: plugins (separate processes,
any language) describe devices with generic capabilities (boolean, level, ...); the daemon maps them
onto Matter device types and clusters and exposes them to controllers (Apple Home, Google Home, Home
Assistant). The Matter protocol itself comes from [rs-matter](https://github.com/project-chip/rs-matter).
The first plugins monitor a Linux desktop (online, user signed in, microphone active, volume, mute).
It follows the conventions of its sibling project paua-terminal.

The plan lives in `docs/design/`; start with [`docs/design/spike.md`](docs/design/spike.md) (what was
proven about rs-matter and why the design looks as it does). It is a young project: expect to refactor.

## Status

Done: Phase 0 (spike), Phase 1 (workspace, config), Phase 2 (plugin host), Phase 3 (the rs-matter
frontend, wired into `it-matters run`) and the plugin supervisor (the daemon starts, restarts and stops
`run` plugins); verified end to end with the matter.js controller, see `docs/testing/e2e.md`. Not built
yet: the TCP + token listener for remote plugins, config reload, the plugin SDK, the Linux plugins,
packaging (systemd units, installer), and verification with Home Assistant / Apple Home / Google Home.

## ⚠️ Rule: exhaustive documentation, kept current (configuration AND usage)

**Every configuration option and every user-visible behaviour MUST be documented exhaustively, with
usage examples, and the documentation MUST be updated in the same change as the code.** A feature,
option, flag, message type, file location, exit code, environment variable or default that is not in
the docs, with an example of using it, is not finished. This applies to every session, human or agent,
now and in the future. Undocumented or stale docs are bugs.

### What "exhaustive" means

1. **Configuration reference (`docs/config/*.md`)**: *every* option has a table row whose first column
   is the full dotted path in backticks (`` `plugins[].run.timeout_ms` ``; `[]` = each list entry,
   `<id>` = each map key) with its type, default, when it applies (live / startup only), validation
   rules and a description that says what it is for. Each page also has a `yaml` example block.
   Index: `docs/config/README.md`. Document inheritance and interactions between options.
2. **Usage guide (`docs/usage.md`)**: every CLI command and flag, exit codes, where files live, the
   environment variables read, how to run it as a service, how to commission and use it with a
   controller, and how to write and run a plugin, each with a copy-paste example. Keep the table of
   commands and flags in step with the code (`CLI_ITEMS` in `crates/it-matters/src/main.rs`).
3. **Plugin protocol (`docs/design/plugin-protocol.md`)**: every message and field, with examples,
   in step with `crates/it-matters-model/src/protocol.rs`. Behaviours plugin authors depend on
   (mapping rules, timeouts, liveness) are documented there.
4. **Examples**: `examples/config.yaml` sets *every* option with a comment. Every `yaml` block in the
   docs and README must be a **valid configuration** (a test loads each one with the real types); use a
   `yml` fence for YAML that is deliberately not a config.
5. **`README.md`** for anything a first-time user needs; **`docs/design/`** for decisions.
6. **Say what exists.** Describe implemented behaviour as such; mark anything planned or not yet
   implemented *explicitly* ("Status: not implemented yet"). When you implement it, remove the marker.

### What you must do on every change

* Adding, renaming, removing or changing the meaning, type, default or validation of an option:
  update its row, the page's example, `examples/config.yaml`, and register any new config struct in
  `all_option_paths()` (`crates/it-matters/src/config.rs`, tests).
* Adding or changing a command, flag, message, file, env var, exit code or behaviour: update
  `docs/usage.md` and/or the protocol doc, with an example.
* Before reporting a change complete, **review the docs against the implementation** (type, actual
  default, validation, live vs startup, examples still run) and list anything you could not verify.
  Passing tests is not proof: the tests only check coverage and that examples parse. The docs tests are
  `docs_cover_every_config_option`, `docs_do_not_describe_options_that_do_not_exist`,
  `yaml_examples_in_docs_are_valid_configs` and `usage_guide_covers_every_cli_item`; a failure means the
  docs are out of date: fix the docs, never the test.
* Run `cargo test --workspace`.

Every config struct has `#[cfg_attr(test, serde(deny_unknown_fields))]` (release builds ignore
unknown keys on purpose so old and new files load; test builds reject them so the example and the
docs cannot hide typos). Keep that attribute on new structs, and `#[serde(default)]`.

## ⚠️ Rule: keep THIRD-PARTY-NOTICES.md up to date

`THIRD-PARTY-NOTICES.md` reproduces the licences of every crate compiled into the daemon. It is
**generated, never edited by hand**:

```bash
python3 -I scripts/update-third-party-notices.py          # regenerate
python3 -I scripts/update-third-party-notices.py --check  # is it current? (CI runs this)
```

Whenever you add, remove or update a dependency, regenerate it and commit the result in the same
change. Do not add a dependency whose licence is not permissive (MIT / Apache-2.0 / BSD / ISC /
Unlicense / Zlib) without asking.

## Build, run, test

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
cargo run -- check --config examples/config.yaml     # validate a config
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy, build, test and the notices check.
Build output must not go to `/tmp` on the author's machine (a small tmpfs); use
`CARGO_TARGET_DIR` elsewhere if a build runs out of space.

## Layout

| Path | What lives there |
|---|---|
| `crates/it-matters-model` | Matter-agnostic device/capability model and plugin wire types (`DeviceSpec`, `CapabilityKind`, `Value`), validation. No IO, no Matter. |
| `crates/it-matters-core` | The host: `supervisor.rs` (starts/restarts/stops `run` plugins: process groups, backoff, output capture), `mapping.rs` (device -> Matter units, level scaling, mute polarity), `allocator.rs` (persisted endpoint ids), `registry.rs` (pure lifecycle state machine returning `FrontendEvent`s), `frontend.rs` (channel boundary + `FakeFrontend`), `host.rs` (actor + session tasks, Unix socket). Tests in `tests/host.rs` use in-memory pipes and virtual time. |
| `crates/it-matters-matter` | The Matter frontend on rs-matter (pinned git rev): `state.rs` (pure endpoint table, unit-tested), `commissioning.rs` (random persisted passcode), `shared.rs` (table + dynamic node metadata + controller-write channel), `node.rs` (endpoint definitions), `handlers.rs` (one async cluster handler per cluster type, serving every endpoint; the event pump lives in `BridgedInfoH::run`), `run.rs` (stack, mDNS via avahi, data model chain). |
| `crates/it-matters` | The daemon binary: CLI (`run`, `check`), `config.rs` (all config types, validation, config + docs tests), `service.rs` (`run`: starts the Matter thread and the tokio host). |
| `docs/config/` | Configuration documentation (see the rule above). `docs/design/` holds design notes and the rs-matter spike. |
| `examples/config.yaml` | The full worked example config. |
| `examples/plugins/demo.py` | A runnable demo plugin (stdlib Python); used in the e2e procedure. |
| `docs/matter.md`, `docs/testing/e2e.md` | What controllers see (and what is verified), and the manual end-to-end procedure. |
| `scripts/update-third-party-notices.py` | Generates `THIRD-PARTY-NOTICES.md`. |

Planned crates (see the plan): `it-matters-matter` (the real frontend on rs-matter), `it-matters-plugin-sdk`,
`plugins/*`. The plugin protocol is documented in `docs/design/plugin-protocol.md`; keep it in step with
`crates/it-matters-model/src/protocol.rs`.

## Conventions and gotchas

- **Rust edition 2024**, rustfmt `max_width = 120`. Prefer small modules with unit tests for pure logic.
- **Never block the core loop.** Every call to an external process or plugin has a hard timeout;
  child processes are killed as a process group. One in-flight job per plugin; drop stale results
  after a config reload.
- **Plugins only run if listed in the main config**: a plugin cannot choose its own executable, and
  nothing read from a per-user or per-directory file may run commands.
- **Remote (non-loopback) plugin listeners require a token file**; this is enforced in config validation.
- **Supervisor tests use real processes and real time** (`/bin/sh` scripts, millisecond settings): paused
  tokio time would let backoffs and grace periods "elapse" while a child is still running. Mutation-check
  changes to it (group kill, SIGKILL escalation, post-exit cleanup, the line cap) as was done originally.
  PR_SET_PDEATHSIG fires when the *thread* that spawned the child exits, which is only safe because the
  daemon runs the supervisor on the main thread of a current-thread runtime: keep it that way.
- **Host design:** one actor task owns the registry, so changes and their frontend events are totally
  ordered; sessions are generic over the stream. The frontend boundary is channels (`async-channel`),
  not async trait calls, because rs-matter will drain it from its own executor. Event channels are
  bounded: a slow frontend slows plugins down instead of growing memory. Never use `pkill -f` with a
  pattern that also appears in your own command line.
- **Host behaviour is specified by tests** (`crates/it-matters-core/tests/host.rs`): when changing the
  host, break the behaviour on purpose (mutation check) to confirm a test fails, as was done for the
  allow-list, one-session-per-plugin, answer ownership, frame limit, idle and set timeouts.
- **Threads:** rs-matter blocks, so `run_blocking` owns its own thread; the host runs on a current-thread
  tokio runtime in `main`. Only `async-channel` channels cross between them. Never call tokio from the
  Matter thread or rs-matter futures from tokio.
- **Matter handlers are singletons per cluster type**, keyed by the request's endpoint id; add a unit kind
  by extending `node.rs`, `handlers.rs`, `state.rs`, the chain in `run.rs`, `mapping.rs` and the docs.
  Cluster attribute and command lists are static per type (`FULL_CLUSTER.with_attrs(...)`): list the
  optional ones you implement explicitly, since `with!(required)` does not work for commands.
- **Linking a binary that uses rs-matter** needs embassy-time's `std` driver and a timer queue
  (`generic-queue-64`); without them the link fails with `__embassy_time_queue_item_from_waker`.
- **The Matter UDP port (5540) is a constant in rs-matter**, not configurable; there is no `matter.port`.
- **Testing the real stack** is manual (`docs/testing/e2e.md`); do it after changing `it-matters-matter`
  or the host/Matter boundary, and update `docs/matter.md`'s verified list. When waiting on a process
  from a shell, use the `[p]attern` trick with `pgrep -f`, or the command matches itself.
- **rs-matter facts** (from the spike): endpoints are sorted by id and a given endpoint's cluster shape
  is fixed for its lifetime, so a device whose capabilities change gets a new endpoint id (allocated
  from a persisted table, never reused); the handler chain is static while metadata is dynamic
  (`Metadata::access` closure), so use one dispatching handler; push state with
  `HandlerContext::notify_*` from a handler's `run`; size the stack's bump arena generously (64 KiB+).
  rs-matter's API is unstable: depend on a pinned git rev.
- Commit messages end with the attribution line required by the session instructions.

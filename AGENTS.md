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

Done: Phase 0 (spike), Phase 1 (workspace, config) and Phase 2 (the plugin host, tested against a fake
Matter side). Not built yet: the rs-matter frontend (`it-matters-matter`), wiring the host into
`it-matters run`, the plugin supervisor (spawning and restarting `run` plugins), the TCP + token
listener for remote plugins, the plugin SDK and the Linux plugins. `it-matters run` still only
validates the config and exits with code 3.

## ⚠️ Rule: keep the configuration documentation up to date

**Whenever you add, rename, remove or change the meaning, type or default of ANY configuration
option, you MUST update the documentation in the same change.** The docs are treated as part of the
feature:

1. **`docs/config/*.md`**: every option has a table row whose first column is the full dotted path in
   backticks (`` `plugins[].run.timeout_ms` ``; `[]` = each list entry, `<id>` = each map key), with
   its type, default, when it applies (live / startup only) and description, in the page for that
   section (index: `docs/config/README.md`). Keep each page's example YAML block correct too.
2. **`examples/config.yaml`**: the worked example. A test loads it with the real types, rejecting
   unknown keys, and validates it, so a typo or a removed option fails.
3. **`README.md`** if it is user-visible behaviour.
4. **`all_option_paths()`** in `crates/it-matters/src/config.rs` (tests): register every new config
   struct with its path prefix. The tests `docs_cover_every_config_option` and
   `docs_do_not_describe_options_that_do_not_exist` check both directions, but they do not verify the
   accuracy of descriptions or defaults.
5. Run `cargo test --workspace`. A failing docs test means the docs are out of date: fix the docs,
   never the test.

Before reporting a config change complete, compare each docs row against the implementation (type,
actual default, validation, whether it applies live or at startup), and say what you could not verify.

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
| `crates/it-matters-core` | The host: `mapping.rs` (device -> Matter units, level scaling, mute polarity), `allocator.rs` (persisted endpoint ids), `registry.rs` (pure lifecycle state machine returning `FrontendEvent`s), `frontend.rs` (channel boundary + `FakeFrontend`), `host.rs` (actor + session tasks, Unix socket). Tests in `tests/host.rs` use in-memory pipes and virtual time. |
| `crates/it-matters` | The daemon binary: CLI (`run`, `check`), `config.rs` (all config types, validation, config + docs tests). |
| `docs/config/` | Configuration documentation (see the rule above). `docs/design/` holds design notes and the rs-matter spike. |
| `examples/config.yaml` | The full worked example config. |
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
- **Host design:** one actor task owns the registry, so changes and their frontend events are totally
  ordered; sessions are generic over the stream. The frontend boundary is channels (`async-channel`),
  not async trait calls, because rs-matter will drain it from its own executor. Event channels are
  bounded: a slow frontend slows plugins down instead of growing memory. Never use `pkill -f` with a
  pattern that also appears in your own command line.
- **Host behaviour is specified by tests** (`crates/it-matters-core/tests/host.rs`): when changing the
  host, break the behaviour on purpose (mutation check) to confirm a test fails, as was done for the
  allow-list, one-session-per-plugin, answer ownership, frame limit, idle and set timeouts.
- **rs-matter facts** (from the spike): endpoints are sorted by id and a given endpoint's cluster shape
  is fixed for its lifetime, so a device whose capabilities change gets a new endpoint id (allocated
  from a persisted table, never reused); the handler chain is static while metadata is dynamic
  (`Metadata::access` closure), so use one dispatching handler; push state with
  `HandlerContext::notify_*` from a handler's `run`; size the stack's bump arena generously (64 KiB+).
  rs-matter's API is unstable: depend on a pinned git rev.
- Commit messages end with the attribution line required by the session instructions.

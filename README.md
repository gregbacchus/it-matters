# it-matters

A lightweight **Matter bridge** written in Rust that hosts **plugins**. Plugins (separate
processes, written in anything) describe devices using generic capabilities: a boolean, a level
(volume), a writable switch (mute). it-matters maps them onto Matter device types and clusters, so
Apple Home, Google Home and Home Assistant can see and control them. The Matter protocol is
provided by [rs-matter](https://github.com/project-chip/rs-matter).

The first use case: expose a Linux desktop to your smart home (is it online, is someone signed in, is
the microphone active, current volume, mute), with the bridge running as a service on an always-on
machine and desktop agents connecting to it.

> **Status: early but working.** The bridge runs: plugins connect over a Unix socket, their devices appear
> in Matter as Contact/Occupancy sensors, On/Off plugs and Speakers, controller writes reach the plugin, and
> devices appear, disappear and go unreachable live. Verified with the matter.js controller on Linux; **not yet
> verified with Home Assistant, Apple Home or Google Home**. The daemon starts, restarts and stops the plugins
> you list. Not built yet: remote (TCP) plugin agents, the Linux desktop plugins and packaging. See
> [`docs/usage.md`](docs/usage.md).

## Try it

Needs a running `avahi-daemon` (see the [usage guide](docs/usage.md) for the full requirements):

```bash
cargo build
cargo run -- check --config examples/config.yaml    # validate the example configuration
cargo test --workspace
```

To run the bridge and pair a controller, follow [`docs/usage.md`](docs/usage.md) (and
[`docs/testing/e2e.md`](docs/testing/e2e.md) for a complete walk-through with a demo plugin).

## Configuration

One YAML file; every option is optional. See [`docs/config/`](docs/config/README.md) and the complete
example [`examples/config.yaml`](examples/config.yaml). `it-matters check` validates a file and lists
every problem.

## Contributing

Contributors and coding agents: see [`AGENTS.md`](AGENTS.md) (notably the rules about keeping the
configuration docs and `THIRD-PARTY-NOTICES.md` current).

MIT licensed; third-party licences are in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

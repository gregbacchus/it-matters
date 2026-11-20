# it-matters

A lightweight **Matter bridge** written in Rust that hosts **plugins**. Plugins (separate
processes, written in anything) describe devices using generic capabilities: a boolean, a level
(volume), a writable switch (mute). it-matters maps them onto Matter device types and clusters, so
Apple Home, Google Home and Home Assistant can see and control them. The Matter protocol is
provided by [rs-matter](https://github.com/project-chip/rs-matter).

The first use case: expose a Linux desktop to your smart home (is it online, is someone signed in, is
the microphone active, current volume, mute), with the bridge running as a service on an always-on
machine and desktop agents connecting to it.

> **Status: early.** The design is proven (see [`docs/design/spike.md`](docs/design/spike.md): rs-matter
> can add and remove bridged devices at runtime) and the workspace, device model and configuration
> exist, but the bridge itself is not implemented yet. `it-matters run` validates the configuration and
> exits.

## Try what exists

```bash
cargo build
cargo run -- check --config examples/config.yaml
cargo test --workspace
```

## Configuration

One YAML file; every option is optional. See [`docs/config/`](docs/config/README.md) and the complete
example [`examples/config.yaml`](examples/config.yaml). `it-matters check` validates a file and lists
every problem.

## Contributing

Contributors and coding agents: see [`AGENTS.md`](AGENTS.md) (notably the rules about keeping the
configuration docs and `THIRD-PARTY-NOTICES.md` current).

MIT licensed; third-party licences are in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

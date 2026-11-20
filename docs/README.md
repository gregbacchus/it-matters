# it-matters documentation

- [Usage guide](usage.md): build, command line, running, writing a plugin, with examples.
- [Configuration](config/README.md): every option of `config.yaml`.
- [Bundled plugins](plugins/README.md): the Linux system and audio plugins, with every flag; [writing a plugin in Rust](plugins/sdk.md).
- [What controllers see](matter.md): the Matter devices, attributes and commands, and what has been verified.
- [End-to-end testing](testing/e2e.md): the manual procedure with a Matter controller.
- [Plugin protocol](design/plugin-protocol.md): every message, mapping and liveness rule.

Design notes live in [`design/`](design/): the [Phase 0 spike](design/spike.md) records what was
learned about rs-matter before the design was fixed.

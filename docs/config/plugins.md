# Plugins and devices

Plugins are separate processes. Locally running plugins connect to the Unix socket in `ipc.socket`; a plugin
listed with `run` is **started and supervised by the daemon**, one listed with `connect` is a remote agent
that connects over TCP. **A plugin only connects if its id is listed here** (anything else is refused with
`plugin ... is not configured`): a plugin cannot choose its own executable or claim an id that is not in
the file.

**Status of `connect`:** remote TCP agents cannot connect yet. The entry is validated, and the daemon logs a
warning at start-up, but nothing listens on `listen`.

## How the daemon runs a `run` plugin

* **Start.** After the plugin socket is bound the daemon starts every `run` plugin, each as
  `command args...`. `command` is looked up in `PATH` if it has no `/`; a relative path is relative to the
  daemon's working directory (which is `/` under systemd), so **prefer absolute paths**.
* **User and environment.** The plugin runs as the daemon's user and inherits the daemon's environment,
  plus two variables: `IT_MATTERS_SOCKET` (the absolute path of `ipc.socket`) and `IT_MATTERS_PLUGIN_ID`
  (the plugin's `id`). A plugin can read these instead of hard-coding either. Standard input is closed.
* **Process group.** Each plugin gets its own process group, so the daemon can stop everything the plugin
  started.
* **Output.** Standard output is logged at `info` and standard error at `warn`, one log line per output
  line, prefixed `[id]`, with log target `plugin` (for example `RUST_LOG=plugin=debug`). Lines longer than
  8 KiB are cut and end with ` [line cut]`.
* **Restarts.** A plugin that exits **for any reason, including a clean exit with status 0**, is started
  again after a delay: 1 s, then doubling (2, 4, 8, ...) up to 60 s. A run that lasted at least 30 s resets
  the delay to 1 s. A command that cannot be started (not found, not executable) is retried the same way
  and logs `cannot start: ...`; it does not stop the daemon. These timings are fixed, not configurable.
  Whenever a plugin exits, everything left in its process group is killed (SIGKILL), so a crashed plugin
  cannot leave helpers running beside its replacement.
* **Reachability.** While a plugin is down its devices are `Reachable = false`; when the restarted plugin
  registers again they return on the **same endpoints** with their last known values.
* **Shutdown.** On `SIGINT`/`SIGTERM` the daemon sends SIGTERM to each plugin's process group, waits up to
  5 s, then sends SIGKILL, before removing the socket and exiting.
* **If the daemon is killed** (`kill -9`, a crash, the OOM killer) it cannot clean up, but the kernel sends
  SIGTERM to each plugin's *main* process, so plugins are not left orphaned. Helper processes a plugin
  started itself are not signalled in that case. The stale socket is replaced on the next start.
* **Plugins started by hand.** A plugin you start yourself can connect with a listed `id` too, but only one
  connection per id is accepted: if a hand-started copy holds the id, the supervised copy is refused
  (`already connected`), exits, and is restarted with growing delays. Run each id one way only.

```yaml
ipc:
  socket: /run/it-matters/plugins.sock
plugins:
  - id: linux-system
    run: { command: /usr/lib/it-matters/plugin-linux-system, timeout_ms: 2000 }
  - id: desktop-agent
    connect: { listen: 0.0.0.0:7550, token_file: /etc/it-matters/agent.token }
devices:
  desktop-audio: { name: Office PC Audio }
```

Each plugin needs exactly one of `run` and `connect`. Plugin ids are unique, 1 to 64 characters of
`a-z`, `0-9`, `-` and `_`.

| Option | Type | Default | Applies | Description |
|---|---|---|---|---|
| `ipc` | map | see below | startup only | The local plugin socket. |
| `ipc.socket` | path | `/run/it-matters/plugins.sock` | startup only | Unix domain socket local plugins connect to. The daemon creates its directory, replaces a stale socket left by a crash (it never deletes anything that is not a socket), sets the socket's permissions to `0660` (owner and group: put plugin users in the daemon's group) and removes it on a clean shutdown. |
| `plugins` | list | `[]` | startup only | The plugins to host. |
| `plugins[]` | map | n/a | startup only | One plugin. |
| `plugins[].id` | string | required | startup only | Unique plugin id. |
| `plugins[].run` | map | none | startup only | A local plugin that the daemon starts and supervises (see above). |
| `plugins[].run.command` | path | required | startup only | Executable to start; must not be empty. Looked up in `PATH` if it contains no `/`. Prefer an absolute path. |
| `plugins[].run.args` | list of strings | `[]` | startup only | Arguments passed to `command`, one list entry per argument (no shell is involved, so no quoting or expansion). |
| `plugins[].run.timeout_ms` | integer | `2000` | startup only | How long the plugin has to answer each write a controller makes (`set`); after that the controller gets a failure. Must be greater than 0. (The handshake timeout is a fixed 5 s.) |
| `plugins[].connect` | map | none | startup only | A TCP listener for a remote plugin agent. **Not implemented yet** (validated only). |
| `plugins[].connect.listen` | string | required | startup only | `host:port` to listen on, e.g. `0.0.0.0:7550`. Each listener address may be used once. **Not used yet.** |
| `plugins[].connect.token_file` | path | none | startup only | File holding the shared secret agents must present. **Required unless `listen` is a loopback address.** Not used yet. |
| `devices` | map | `{}` | startup only | Overrides keyed by device id (the id a plugin registered). |
| `devices.<id>` | map | n/a | startup only | Overrides for one device. |
| `devices.<id>.name` | string | none | startup only | Name shown in controllers instead of the plugin's own device name. For a device made of several units the unit's id is appended (`Office PC online`). Must not be empty. |

# Plugins and devices

Plugins are separate processes. Locally running plugins connect to the Unix socket in `ipc.socket`;
a plugin listed with `run` is meant to be started and supervised by the daemon, one listed with
`connect` is a remote agent that connects over TCP. **A plugin only connects if its id is listed
here** (anything else is refused with `plugin ... is not configured`): a plugin cannot choose its own
executable or claim an id that is not in the file.

**Status of `run` and `connect` (read this):**

* `run`: **the daemon does not start plugins yet.** Today a `run` entry only *allows* a plugin with that
  `id` to connect to `ipc.socket` and sets its `timeout_ms`; `command` and `args` are validated but not
  used. Start the plugin yourself (a shell, a systemd unit, ...). Supervision (starting, restarting with
  backoff, killing the process group) is not implemented.
* `connect`: **remote TCP agents cannot connect yet.** The entry is validated, and the daemon logs a
  warning at start-up, but nothing listens on `listen`.

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
| `plugins[].run` | map | none | startup only | A local plugin. Its `timeout_ms` applies today; the daemon will start `command` itself once supervision exists (not yet). |
| `plugins[].run.command` | path | required | startup only | Executable to start. Must not be empty. **Not used yet** (the daemon does not start plugins). |
| `plugins[].run.args` | list of strings | `[]` | startup only | Arguments for `command`. **Not used yet.** |
| `plugins[].run.timeout_ms` | integer | `2000` | startup only | Hard timeout for the plugin's handshake and for each write it is asked to perform. Must be greater than 0. |
| `plugins[].connect` | map | none | startup only | A TCP listener for a remote plugin agent. **Not implemented yet** (validated only). |
| `plugins[].connect.listen` | string | required | startup only | `host:port` to listen on, e.g. `0.0.0.0:7550`. Each listener address may be used once. **Not used yet.** |
| `plugins[].connect.token_file` | path | none | startup only | File holding the shared secret agents must present. **Required unless `listen` is a loopback address.** Not used yet. |
| `devices` | map | `{}` | startup only | Overrides keyed by device id (the id a plugin registered). |
| `devices.<id>` | map | n/a | startup only | Overrides for one device. |
| `devices.<id>.name` | string | none | startup only | Name shown in controllers instead of the plugin's own device name. For a device made of several units the unit's id is appended (`Office PC online`). Must not be empty. |

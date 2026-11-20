# Plugins and devices

Plugins are separate processes. Locally running plugins connect to the Unix socket in `ipc.socket`;
a plugin listed with `run` is started and supervised by the daemon, one listed with `connect` is a
remote agent that connects over TCP. **A plugin only runs if it is listed here**: a plugin cannot
choose its own executable.

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
| `ipc.socket` | path | `/run/it-matters/plugins.sock` | startup only | Unix domain socket local plugins connect to. |
| `plugins` | list | `[]` | startup only | The plugins to host. |
| `plugins[]` | map | n/a | startup only | One plugin. |
| `plugins[].id` | string | required | startup only | Unique plugin id. |
| `plugins[].run` | map | none | startup only | A plugin process the daemon starts and supervises. |
| `plugins[].run.command` | path | required | startup only | Executable to start. Must not be empty. |
| `plugins[].run.args` | list of strings | `[]` | startup only | Arguments. |
| `plugins[].run.timeout_ms` | integer | `2000` | startup only | Hard timeout for the plugin's handshake and for each write it is asked to perform. Must be greater than 0. |
| `plugins[].connect` | map | none | startup only | A TCP listener for a remote plugin agent. |
| `plugins[].connect.listen` | string | required | startup only | `host:port` to listen on, e.g. `0.0.0.0:7550`. Each listener address may be used once. |
| `plugins[].connect.token_file` | path | none | startup only | File holding the shared secret agents must present. **Required unless `listen` is a loopback address.** |
| `devices` | map | `{}` | startup only | Overrides keyed by device id (the id a plugin registered). |
| `devices.<id>` | map | n/a | startup only | Overrides for one device. |
| `devices.<id>.name` | string | none | startup only | Name shown in controllers instead of the plugin's own. Must not be empty. |

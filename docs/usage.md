# Usage guide

How to build, configure, run and extend it-matters. Each section starts with its **status**: what
works today and what is planned. The configuration options themselves are in
[`config/README.md`](config/README.md); the plugin wire protocol is in
[`design/plugin-protocol.md`](design/plugin-protocol.md).

## Build and install

Status: works.

```bash
sudo apt install build-essential          # Rust 1.92 or newer is also required
cargo build --release                     # builds target/release/it-matters
sudo install -m 0755 target/release/it-matters /usr/local/bin/it-matters
it-matters --version
```

## Command line

Status: `check`, `--help` and `--version` work. `run` validates the configuration, then stops with
exit code 3 because the Matter bridge is not wired up yet.

```
it-matters [--config <file>] [run]
it-matters [--config <file>] check
it-matters --help | --version
```

| Command or flag | Description |
|---|---|
| `run` | Run the bridge. This is the default when no command is given. *Not implemented yet:* it loads and validates the configuration, then exits with code 3. |
| `check` | Load and validate the configuration and exit: `0` if it is valid, `1` with every problem listed otherwise. Use it in deployment scripts and before restarting the service. |
| `--config <file>`, `-c` | Use this configuration file. The file must exist; the search below is skipped. |
| `--help`, `-h` | Print the usage text and exit with `0`. |
| `--version`, `-V` | Print `it-matters <version>` and exit with `0`. |

Examples:

```bash
it-matters check                                   # validate the configuration found by the search
it-matters --config /etc/it-matters/config.yaml check
it-matters -c ./my.yaml check && echo "ready to deploy"
it-matters --version
```

### Exit codes

| Code | Meaning |
|---|---|
| `0` | Success (`check` found a valid configuration; `--help`, `--version`). |
| `1` | The configuration file could not be read or parsed, or failed validation. Every problem is printed to standard error. |
| `2` | Unknown command line argument (the usage text is printed). |
| `3` | `run` reached the point where the bridge would start, but it is not implemented yet. |

### Where the configuration is read from

The first match wins; with no file at all, the built-in defaults apply (an empty configuration is valid).

1. the file given with `--config` (an error if it does not exist);
2. `$XDG_CONFIG_HOME/it-matters/config.yaml`, or `~/.config/it-matters/config.yaml` when
   `XDG_CONFIG_HOME` is unset or empty;
3. `/etc/it-matters/config.yaml` (the location for a system service).

Environment variables read: `XDG_CONFIG_HOME` and `HOME` (to find the configuration). There are no
others.

## Configuration in practice

Status: works (parsing, defaults and validation); most options take effect once `run` is implemented.

Every option is optional. The smallest useful file names the bridge and one plugin that the daemon
starts itself:

```yaml
bridge:
  name: Home Bridge
plugins:
  - id: linux-system
    run:
      command: /usr/lib/it-matters/plugin-linux-system
```

A bridge that accepts a remote desktop agent over TCP (a non-loopback address requires a token file):

```yaml
plugins:
  - id: desktop-agent
    connect:
      listen: 0.0.0.0:7550
      token_file: /etc/it-matters/agent.token
```

Renaming a device that a plugin registered (the key is the plugin's device id):

```yaml
devices:
  desktop-audio:
    name: Office PC Audio
```

Validate after every edit:

```bash
it-matters --config ./config.yaml check
```

A complete file that sets every option is [`examples/config.yaml`](../examples/config.yaml).

## Running as a service

Status: not implemented yet (there is no systemd unit or installer, and `run` does not start the
bridge). Planned: a system service for the bridge (reading `/etc/it-matters/config.yaml`, state in
`/var/lib/it-matters`, plugin socket in `/run/it-matters`) and a user service for session plugins.

## Commissioning and controllers

Status: not implemented yet (needs the Matter frontend). Planned: `it-matters run` prints the pairing
code and QR code on first start; add the bridge in Apple Home, Google Home or Home Assistant with it.
The bridge then appears with one bridged device per plugin unit.

## Writing a plugin

Status: the host side of the protocol is implemented and tested; `run` does not serve it yet, so a
plugin cannot connect to a running daemon yet. The protocol is final for v1.

A plugin is any program that connects to the Unix socket (`ipc.socket`, default
`/run/it-matters/plugins.sock`) and exchanges one JSON object per line. Its `plugin` id in `hello` must
be listed under `plugins` in the daemon's configuration. A complete session, with `>` for what the
plugin sends and `<` for what the host answers:

```yml
> {"type":"hello","plugin":"demo","version":"0.1.0","proto":1}
< {"type":"welcome","proto":1}
> {"type":"register","device":{"id":"desk-lamp","name":"Desk Lamp","capabilities":[{"id":"power","type":"boolean","writable":true}]}}
> {"type":"update","device":"desk-lamp","capability":"power","value":true}
< {"type":"set","req_id":1,"device":"desk-lamp","capability":"power","value":false}
> {"type":"set_result","req_id":1,"ok":true}
```

The last two lines are a controller switching the lamp off: the host sends `set`, the plugin applies it
and answers with `set_result` within its `timeout_ms`. A minimal Python plugin:

```python
import json, socket

sock = socket.socket(socket.AF_UNIX)
sock.connect("/run/it-matters/plugins.sock")
f = sock.makefile("rw")

def send(msg):
    f.write(json.dumps(msg) + "\n")
    f.flush()

send({"type": "hello", "plugin": "demo", "version": "0.1.0", "proto": 1})
print(f.readline())                          # {"type":"welcome","proto":1}
send({"type": "register", "device": {"id": "desk-lamp", "name": "Desk Lamp",
      "capabilities": [{"id": "power", "type": "boolean", "writable": True}]}})
send({"type": "update", "device": "desk-lamp", "capability": "power", "value": True})
for line in f:                               # answer controller writes and heartbeats
    msg = json.loads(line)
    if msg["type"] == "set":
        send({"type": "set_result", "req_id": msg["req_id"], "ok": True})
    elif msg["type"] == "ping":
        send({"type": "pong"})
```

The full message reference, the device mapping rules and the liveness rules are in
[`design/plugin-protocol.md`](design/plugin-protocol.md).

## Troubleshooting

* `invalid configuration: ...` : run `it-matters check` and fix each listed problem; every problem is
  reported at once.
* `<file>: No such file or directory` with `--config`: an explicit file must exist.
* `the Matter bridge is not implemented yet` (exit 3): expected for now, see *Command line*.

# Usage guide

How to build, configure, run and extend it-matters. Each section starts with its **status**: what
works today and what is planned. The configuration options themselves are in
[`config/README.md`](config/README.md); what controllers see is in [`matter.md`](matter.md); the plugin
wire protocol is in [`design/plugin-protocol.md`](design/plugin-protocol.md).

## Requirements

* Linux, Rust 1.92 or newer and a C toolchain to build (`sudo apt install build-essential`).
* To **run the bridge**: a running **avahi-daemon** (Matter controllers find the bridge through mDNS, and
  the bridge talks to avahi over the system D-Bus), a network with IPv6 enabled (link-local is enough),
  and UDP port 5540 open to your controllers.

## Build and install

Status: works.

```bash
cargo build --release                     # builds target/release/it-matters
sudo install -m 0755 target/release/it-matters /usr/local/bin/it-matters
it-matters --version
```

The first build compiles rs-matter and about 200 other crates (a few minutes; a debug build needs several
GB of disk).

## Command line

Status: works.

```
it-matters [--config <file>] [run]
it-matters [--config <file>] check
it-matters --help | --version
```

| Command or flag | Description |
|---|---|
| `run` | Run the bridge until it receives `SIGINT` or `SIGTERM`. This is the default when no command is given. It validates the configuration, starts the Matter stack, binds the plugin socket and serves plugins. |
| `check` | Load and validate the configuration and exit: `0` if it is valid, `1` with every problem listed otherwise. Use it in deployment scripts and before restarting the service. |
| `--config <file>`, `-c` | Use this configuration file. The file must exist; the search below is skipped. |
| `--help`, `-h` | Print the usage text and exit with `0`. |
| `--version`, `-V` | Print `it-matters <version>` and exit with `0`. |

Examples:

```bash
it-matters check                                   # validate the configuration found by the search
it-matters --config /etc/it-matters/config.yaml check
it-matters -c ./my.yaml check && echo "ready to deploy"
RUST_LOG=info it-matters --config ./my.yaml run    # run in the foreground, logging to standard error
it-matters --version
```

### Logging

Log lines go to standard error. The level comes from `RUST_LOG` and defaults to `info`; for example
`RUST_LOG=debug`, or `RUST_LOG=it_matters=debug,rs_matter=warn` to see more of the daemon and less of
the Matter stack. At `info` the daemon logs the pairing codes (until commissioned), the plugin socket,
each plugin it is waiting for, refused controller writes (with the reason) and shutdown.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | Success: `check` found a valid configuration; `--help`, `--version`; `run` stopped after `SIGINT`/`SIGTERM`. |
| `1` | The configuration file could not be read or parsed, or failed validation (every problem is printed to standard error); or `run` failed: for example the plugin socket cannot be bound, the state directory is unusable, avahi/D-Bus is unreachable, or the Matter stack stopped. |
| `2` | Unknown command line argument (the usage text is printed). |

### Where the configuration is read from

The first match wins; with no file at all, the built-in defaults apply (an empty configuration is valid).

1. the file given with `--config` (an error if it does not exist);
2. `$XDG_CONFIG_HOME/it-matters/config.yaml`, or `~/.config/it-matters/config.yaml` when
   `XDG_CONFIG_HOME` is unset or empty;
3. `/etc/it-matters/config.yaml` (the location for a system service).

Environment variables read: `XDG_CONFIG_HOME` and `HOME` (to find the configuration) and `RUST_LOG` (the
log level). There are no others. The configuration is read once at start; **restart the daemon after
changing it**.

## Configuration in practice

Status: works.

Every option is optional. The smallest useful file names the bridge and allows one plugin to connect:

```yaml
bridge:
  name: Home Bridge
plugins:
  - id: demo
    run:
      command: /usr/local/bin/my-plugin
```

(`command` is required by the validation but the daemon does not start plugins yet, see
[plugins](config/plugins.md): start your plugin yourself and it connects with the id `demo`.)

Renaming a device that a plugin registered (the key is the plugin's device id):

```yaml
devices:
  desktop-audio:
    name: Office PC Audio
```

Keeping state somewhere else, and pinning the pairing codes (for testing only; see
[matter](config/matter.md#pairing-codes)):

```yaml
matter:
  storage_dir: /srv/it-matters
  passcode: 20202021
  discriminator: 3840
```

Validate after every edit:

```bash
it-matters --config ./config.yaml check
```

A complete file that sets every option is [`examples/config.yaml`](../examples/config.yaml).

## Running the bridge

Status: works in the foreground. Not packaged: there is no installer, no systemd unit shipped, and the
daemon does not start plugins.

```bash
sudo mkdir -p /var/lib/it-matters /run/it-matters && sudo chown $USER /var/lib/it-matters /run/it-matters
RUST_LOG=info it-matters --config ./config.yaml run
```

On start the daemon creates its state directory (`matter.storage_dir`), binds the plugin socket
(`ipc.socket`, mode `0660`), starts the Matter stack and logs `plugins connect to <socket>`. If it is not
yet commissioned it also logs the pairing codes. `Ctrl-C` or `kill -TERM` stops it cleanly.

An example systemd unit (**an untested sketch, not shipped**; adjust paths and the user):

```yml
[Unit]
Description=it-matters Matter bridge
After=network-online.target avahi-daemon.service
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/it-matters --config /etc/it-matters/config.yaml run
User=it-matters
StateDirectory=it-matters
RuntimeDirectory=it-matters
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

## Commissioning and controllers

Status: works with the matter.js controller; **not yet verified** with Home Assistant, Apple Home or Google
Home (see [what has been verified](matter.md#what-has-been-verified)).

1. Start the bridge and find the pairing codes in the log:

   ```
   SetupQRCode: [MT:Y.K90YDA13I7D847E3P0WISA0DK5N1K8SQ1RYCU1O0]
   PairingCode: [2499-520-8318]
   ```

   They are printed only while the bridge is not commissioned. On first start the passcode and discriminator
   are generated randomly (or taken from `matter.passcode` / `matter.discriminator`); generated ones are
   kept in `<storage_dir>/commissioning.json` so the code stays the same across restarts.
2. In your controller's app choose *Add device / Add Matter accessory* and enter the code or scan the QR
   code. The bridge uses test attestation certificates, so a controller may say the accessory is not
   certified; accept it only for a bridge you started yourself.
3. The bridge then appears with one device per plugin unit (see [what controllers see](matter.md)). Devices
   appear and disappear as plugins register and unregister, without re-pairing.

To pair again from scratch, stop the daemon and delete `matter.storage_dir`.
A step-by-step procedure with the matter.js controller is in [`testing/e2e.md`](testing/e2e.md).

## Writing a plugin

Status: works. Plugins connect to the Unix socket; the daemon does not start them for you yet.

A plugin is any program that connects to the Unix socket (`ipc.socket`, default
`/run/it-matters/plugins.sock`) and exchanges one JSON object per line. Its `plugin` id in `hello` must
be listed under `plugins` in the daemon's configuration, otherwise the host answers with an `error` and
closes the connection. A complete session, with `>` for what the plugin sends and `<` for what the host
answers:

```yml
> {"type":"hello","plugin":"demo","version":"0.1.0","proto":1}
< {"type":"welcome","proto":1}
> {"type":"register","device":{"id":"desk-lamp","name":"Desk Lamp","capabilities":[{"id":"power","type":"boolean","writable":true}]}}
> {"type":"update","device":"desk-lamp","capability":"power","value":true}
< {"type":"set","req_id":1,"device":"desk-lamp","capability":"power","value":false}
> {"type":"set_result","req_id":1,"ok":true}
```

The last two lines are a controller switching the lamp off: the host sends `set`, the plugin applies it
and answers with `set_result` within its `timeout_ms`.

A complete, runnable example in Python (standard library only) is
[`examples/plugins/demo.py`](../examples/plugins/demo.py): it registers a speaker and two sensors, answers
controller writes and heartbeats, and flips a "microphone" sensor every 20 seconds.

```bash
python3 examples/plugins/demo.py /run/it-matters/plugins.sock demo     # socket path, then plugin id
```

The full message reference, the device mapping rules and the liveness rules are in
[`design/plugin-protocol.md`](design/plugin-protocol.md).

## Troubleshooting

* `invalid configuration: ...`: run `it-matters check` and fix each listed problem; every problem is
  reported at once.
* `<file>: No such file or directory` with `--config`: an explicit file must exist.
* `cannot reach the system D-Bus (is avahi-daemon running?)`: install and start avahi
  (`sudo apt install avahi-daemon`, `systemctl status avahi-daemon`).
* `<socket>: ... exists and is not a socket`: `ipc.socket` points at a regular file; the daemon refuses to
  delete it. Choose another path.
* `Permission denied` on the socket or state directory: the daemon's user needs write access to
  `ipc.socket`'s directory and to `matter.storage_dir`.
* A plugin gets `plugin "x" is not configured`: add `x` under `plugins` in the configuration and restart.
* A plugin gets `plugin "x" is already connected`: only one connection per plugin id is accepted; a dead
  connection is dropped when the plugin process exits or after 30 seconds of silence.
* Devices are shown as "not responding" in the controller: the plugin is not connected (`Reachable` is
  false). Check that it is running and using the right socket path and plugin id.
* The controller cannot find the bridge: check avahi, that UDP 5540 is not blocked, and that the
  controller is on the same network segment. On machines with VPN interfaces give the controller the right
  interface.

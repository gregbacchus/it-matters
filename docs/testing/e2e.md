# End-to-end testing with a Matter controller

Automated tests (`cargo test --workspace`) cover everything except the real Matter network. This is the
manual procedure that exercises the whole stack: the daemon, a plugin, rs-matter, mDNS and a controller.
It has not been automated because it needs a running avahi daemon, the system D-Bus and a controller.

Requirements: the machine's `avahi-daemon` running, a built `it-matters` (`cargo build`), Python 3, and
Node.js 20+ for the controller.

## 1. A throwaway configuration

```yaml
bridge:
  name: E2E Bridge
matter:
  storage_dir: /tmp/it-matters-e2e/state     # use a directory on a real disk if /tmp is small
ipc:
  socket: /tmp/it-matters-e2e/run/plugins.sock
plugins:
  - id: demo
    run:
      command: python3                       # the daemon starts the demo plugin itself
      args: [/path/to/it-matters/examples/plugins/demo.py]
devices:
  desktop-audio:
    name: Office PC Audio
```

```bash
mkdir -p /tmp/it-matters-e2e
it-matters --config /tmp/it-matters-e2e/config.yaml check
```

## 2. Start the daemon (it starts the demo plugin)

```bash
RUST_LOG=info it-matters --config /tmp/it-matters-e2e/config.yaml run 2> /tmp/it-matters-e2e/daemon.log &
grep -E "PairingCode|SetupQRCode" /tmp/it-matters-e2e/daemon.log     # the manual pairing code, e.g. 2499-520-8318
grep "\[demo\]" /tmp/it-matters-e2e/daemon.log                      # started (pid N), connected ..., the plugin's output
```

## 3. Commission and read with the matter.js shell

```bash
mkdir controller && cd controller && npm install @matter/nodejs-shell@0.18.0
SHELL_JS=node_modules/@matter/nodejs-shell/dist/esm/app.js
# Pick the network interface your LAN uses with --netInterface (needed on machines with VPN interfaces):
node $SHELL_JS 1 --netInterface wlp195s0
```

At the `matter>` prompt (the shell is interactive; `exit` leaves it):

```
commission pair 11 --pairingCode 2499-520-8318
nodes connect 11
attributes descriptor read partslist 11 1 --remote           # [2,3,4]
attributes levelcontrol read currentlevel 11 2 --remote      # 76 (the plugin's volume 30 of 100, scaled to 0..254)
attributes onoff read onoff 11 2 --remote                    # true: not muted
attributes bridgeddevicebasicinformation read nodelabel 11 2 --remote   # "Office PC Audio"
attributes occupancysensing read occupancy 11 4 --remote
commands levelcontrol movetolevel '{"level":127,"transitionTime":0,"optionsMask":{},"optionsOverride":{}}' 11 2
commands onoff off 11 2                                       # the plugin prints: controller set mute = True
```

`--remote` reads from the bridge instead of the shell's cache. The shell keeps its own copy of the
bridge's structure from when it connected, so an endpoint added later is only readable after pairing
again; read the Aggregator's parts list (endpoint 1) to see topology changes.

## 4. Things worth checking

* `kill -9` the plugin (its pid is in the `started (pid N)` log line): `Reachable` becomes `false` on
  endpoints 2 to 4 and `commands onoff off 11 2` fails; about a second later the daemon restarts the plugin
  (`exited (signal: 9 (SIGKILL))`, `restarting in 1.0s`, `started`), `Reachable` is `true` again and the
  endpoint ids are unchanged.
* Commission *before* the plugin connects (for example with `plugins: []`, then add the plugin and restart
  the daemon): the parts list changes from `[]` to `[2,3,4]`.
* `kill -9` the daemon: the plugin is not left running (the kernel sends it SIGTERM); the stale socket is
  replaced on the next start.
* Stop the daemon with `kill -TERM`: it logs `shutting down` and `[demo] stopped`, the plugin is gone, and the
  socket is removed.

## Notes

* Do not use `pkill -f`/`pgrep -f` with a pattern that also appears in your own command line: it matches
  the shell running it. Write the pattern as `e2e/[p]lugin.py` (the bracket stops it matching itself).
* A debug build needs a few GB of disk for rs-matter; build with `CARGO_TARGET_DIR` on a large disk.

## Testing the Linux plugins (without touching your real audio)

The bundled [Linux plugins](../plugins/README.md) read your real desktop (sign-in state, microphone use) and
control the speaker. To test the **writes** safely, point the audio plugin at a **disposable null speaker**
instead of your real one. It is created with `pw-cli` and exists only while that process runs; the lowest
priority keeps WirePlumber from ever making it the default:

```bash
# Keep pw-cli alive for 15 minutes (it owns the speaker); detach it from the terminal.
setsid nohup bash -c "(echo 'create-node adapter { factory.name=support.null-audio-sink node.name=it-matters-test-sink \
media.class=Audio/Sink audio.position=[FL,FR] priority.session=1 priority.driver=1 object.linger=false }'; sleep 900) | pw-cli" \
  > /dev/null 2>&1 < /dev/null &
wpctl get-volume @DEFAULT_AUDIO_SINK@     # note your real volume; it must be unchanged at the end
```

Configuration (adjust the paths; `--sink` takes the node name):

```yaml
plugins:
  - id: linux-system
    run:
      command: /path/to/target/debug/it-matters-plugin-linux-system
      args: [--device, e2e-pc, --name, E2E PC]
  - id: linux-audio
    run:
      command: /path/to/target/debug/it-matters-plugin-linux-audio
      args: [--device, e2e-pc-audio, --name, E2E PC Audio, --sink, it-matters-test-sink, --poll-ms, "500"]
```

After commissioning (section 3) the controller sees a Speaker (volume and mute), an Occupancy Sensor
(microphone) and two Contact Sensors (online, signed in); read the endpoint ids from the shell's
`ready endpoint#` lines. Then:

* `attributes booleanstate read statevalue <node> <signed-in endpoint> --remote`: `true` on a desktop session.
* Microphone: while `timeout 6 pw-cat --record /dev/null` runs (a capture from the default microphone to
  nowhere), `attributes occupancysensing read occupancy <node> <mic endpoint> --remote` is `{"occupied":true}`;
  after it ends, `false`. Capturing the speaker's monitor (`pw-cat --record -P stream.capture.sink=true
  --target <sink id> /dev/null`) does **not** turn it on.
* `commands levelcontrol movetolevel '{"level":127,"transitionTime":0,"optionsMask":{},"optionsOverride":{}}' <node> <speaker endpoint>`
  makes `wpctl get-volume <test sink id>` print `Volume: 0.50`; `commands onoff off ...` shows `[MUTED]`,
  `commands onoff toggle ...` unmutes; `commands levelcontrol step '{"stepMode":0,"stepSize":25,...}' ...`
  raises it by about 10 %.

Clean up and check nothing of yours changed:

```bash
kill -TERM <daemon pid>                    # the plugins stop with it
kill $(pgrep -x pw-cli)                    # removes the test speaker
wpctl get-volume @DEFAULT_AUDIO_SINK@      # unchanged
```

(WirePlumber remembers a volume per node name, so a test speaker of the same name starts at whatever you last
set it to.) The automated `#[ignore]`d tests do the same safely by themselves: see
[`plugins/linux-audio.md`](../plugins/linux-audio.md#testing).

## Testing remote agents (TCP)

Run the bridge with a `connect` entry and the plugin as a remote agent (here on the same machine, through the
machine's LAN address so the non-loopback path is used; replace the address with yours):

```yaml
plugins:
  - id: desktop
    connect:
      listen: 192.168.1.10:7550
      token_file: /tmp/it-matters-e2e/desktop.token
```

```bash
openssl rand -hex 32 > /tmp/it-matters-e2e/desktop.token && chmod 600 /tmp/it-matters-e2e/desktop.token
it-matters --config /tmp/it-matters-e2e/config.yaml check        # also checks the token; warns the port is unencrypted
RUST_LOG=info it-matters --config /tmp/it-matters-e2e/config.yaml run 2> /tmp/it-matters-e2e/daemon.log &
it-matters-plugin-linux-system --device e2e-pc --name "E2E PC" --connect 192.168.1.10:7550 \
    --plugin-id desktop --token-file /tmp/it-matters-e2e/desktop.token &
python3 examples/plugins/demo.py --connect 192.168.1.10:7550 --plugin-id desktop --token-file /tmp/it-matters-e2e/desktop.token
```

(Run one agent per entry: a second connection for `desktop` replaces the first.) Things worth checking:

* A wrong token (`--token-file` with other contents) or a wrong `--plugin-id` is refused with
  `authentication failed`; the daemon log has a `host: authentication failed ... from <address>` warning.
* **Power loss:** (get the pid with a pattern that cannot match your own shell, e.g. `pgrep -f '[i]t-matters-plugin-linux-system --device'`, and check `/proc/<pid>/comm`; freezing your own shell proves nothing) `kill -STOP <agent pid>` freezes the agent without closing its connection, like a computer
  losing power. After about 30 seconds the devices' `Reachable` becomes `false` (the log shows nothing: the
  bridge dropped the silent connection). `kill -CONT <agent pid>`: the agent notices its connection is gone,
  reconnects after a second or two, and the devices are back **on the same endpoints**.
* `kill -9 <agent pid>` (a crashed agent) is noticed at once, because the operating system closes its socket.
* Restart the bridge: the agent logs `cannot connect`, retries with growing delays, and reconnects when the
  bridge is back.

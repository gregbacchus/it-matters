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
      command: /bin/true                     # not used yet: the daemon does not start plugins
devices:
  desktop-audio:
    name: Office PC Audio
```

```bash
mkdir -p /tmp/it-matters-e2e
it-matters --config /tmp/it-matters-e2e/config.yaml check
```

## 2. Start the daemon and the demo plugin

```bash
RUST_LOG=info it-matters --config /tmp/it-matters-e2e/config.yaml run 2> /tmp/it-matters-e2e/daemon.log &
grep -E "PairingCode|SetupQRCode" /tmp/it-matters-e2e/daemon.log     # the manual pairing code, e.g. 2499-520-8318
python3 examples/plugins/demo.py /tmp/it-matters-e2e/run/plugins.sock demo &
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

* Stop the plugin: `Reachable` becomes `false` on endpoints 2 to 4 and `commands onoff off 11 2` fails.
  Start it again: `Reachable` is `true` and the endpoint ids are unchanged.
* Start the plugin *after* commissioning: the parts list changes from `[]` to `[2,3,4]`.
* Stop the daemon with `kill -TERM`: it logs `shutting down`, exits and removes the socket.

## Notes

* Do not use `pkill -f`/`pgrep -f` with a pattern that also appears in your own command line: it matches
  the shell running it. Write the pattern as `e2e/[p]lugin.py` (the bracket stops it matching itself).
* A debug build needs a few GB of disk for rs-matter; build with `CARGO_TARGET_DIR` on a large disk.

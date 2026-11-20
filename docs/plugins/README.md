# Bundled plugins

it-matters ships two plugins for a Linux desktop. Each is a separate program that the daemon starts
(`run` in the [configuration](../config/plugins.md)) and that connects to the daemon's plugin socket.

| Plugin | What it reports | Page |
|---|---|---|
| `it-matters-plugin-linux-system` | The computer is online; a user is signed in (systemd-logind) | [linux-system.md](linux-system.md) |
| `it-matters-plugin-linux-audio` | The speaker's volume and mute (writable); whether a microphone is in use (PipeWire) | [linux-audio.md](linux-audio.md) |

To write your own, see [the Rust SDK](sdk.md) (or the [protocol](../design/plugin-protocol.md) for any other
language).

## Building and installing

`cargo build --release` builds the daemon and both plugins into `target/release/`. Install them next to the
daemon:

```bash
cargo build --release
sudo install -m 0755 target/release/it-matters target/release/it-matters-plugin-linux-system \
    target/release/it-matters-plugin-linux-audio /usr/local/bin/
```

## Using them

```yaml
plugins:
  - id: linux-system
    run:
      command: /usr/local/bin/it-matters-plugin-linux-system
      args: [--device, office-pc, --name, Office PC]
  - id: linux-audio
    run:
      command: /usr/local/bin/it-matters-plugin-linux-audio
      args: [--device, office-pc-audio, --name, Office PC Audio]
```

Plugin `id`s and device ids (`--device`) are independent: the `id` is how the plugin is listed in the
configuration, the device id names what it registers (and the endpoint ids are remembered by it).

## Running on a desktop: as the desktop user

The audio plugin talks to the *user's* PipeWire session, which exists only for that user (it needs the
user's `XDG_RUNTIME_DIR`, normally `/run/user/<uid>`). The daemon passes its own environment on to the
plugins it starts, so **run the daemon as the desktop user, from that user's session** (a terminal, or a
systemd *user* unit), not as a system service owned by another user. The system plugin (logind) works from
either, because the system D-Bus is readable by every user.

An example user unit (**an untested sketch, not shipped**), saved as `~/.config/systemd/user/it-matters.service`
and enabled with `systemctl --user enable --now it-matters`:

```yml
[Unit]
Description=it-matters Matter bridge
After=avahi-daemon.service

[Service]
ExecStart=/usr/local/bin/it-matters --config %h/.config/it-matters/config.yaml run
Restart=on-failure

[Install]
WantedBy=default.target
```

A user unit stops when the user logs out unless lingering is enabled (`loginctl enable-linger`). With the bridge
on the same machine "online" is only ever true while the computer runs; to see a computer's *power state* from
an always-on bridge, run the plugins as remote agents (next section).

## Running on another machine as a remote agent

Run the bridge on an always-on machine (a Raspberry Pi, a server) and the plugins on each desktop, connecting
over TCP. When a desktop is switched off or loses its network its devices turn **unreachable within about 30
seconds**, which is how you notice a computer going away. Setting it up:

1. **On the bridge**, make a token and add a `connect` entry for the agent (one entry, port and token per agent):

   ```bash
   openssl rand -hex 32 > /etc/it-matters/desktop.token && chmod 600 /etc/it-matters/desktop.token
   ```

   ```yaml
   plugins:
     - id: desktop
       connect:
         listen: 192.168.1.10:7550          # the bridge machine's address on your LAN or VPN
         token_file: /etc/it-matters/desktop.token
   ```

   Run `it-matters check`, restart the daemon, and open TCP 7550 to the desktop only.
2. **Copy the token file to the desktop** (`scp`), keep it private (`chmod 600`).
3. **On the desktop**, run the plugin with `--connect`, `--plugin-id` (the `id` above) and `--token-file`
   (flags are on each plugin's page), under a systemd *user* unit so it starts at login and restarts:

   ```yml
   [Service]
   ExecStart=/usr/local/bin/it-matters-plugin-linux-system --device office-pc --name "Office PC" \
       --connect 192.168.1.10:7550 --plugin-id desktop --token-file %h/.config/it-matters/desktop.token
   Restart=always
   ```

   (An untested sketch.) The plugin also reconnects by itself, so `Restart=` only matters if the process dies.

If the connection drops, the plugin reconnects after 1, 2, 4, ... up to 30 seconds, and its devices return on the
**same endpoints**. **The connection is not encrypted**: use a trusted LAN or a tunnel; see what the token does
and does not protect in [the configuration reference](../config/plugins.md#what-this-protects-and-what-it-does-not).
The audio plugin still needs the desktop user's PipeWire session, so on the desktop run it as that user.

## Testing

Both plugins have unit tests and loop tests against the real host with fake backends (`cargo test`), and
`#[ignore]`d tests against the real system (`cargo test -- --ignored`). The ignored tests are described on
each plugin's page; none changes your real audio settings.

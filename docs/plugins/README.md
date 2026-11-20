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

A user unit stops when the user logs out unless lingering is enabled (`loginctl enable-linger`). The bridge
being on the same machine means "online" is only ever true while the computer runs; to see a computer's
*power state* from another always-on bridge you need remote agents, which are not implemented yet.

## Testing

Both plugins have unit tests and loop tests against the real host with fake backends (`cargo test`), and
`#[ignore]`d tests against the real system (`cargo test -- --ignored`). The ignored tests are described on
each plugin's page; none changes your real audio settings.

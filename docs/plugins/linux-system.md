# `it-matters-plugin-linux-system`

Reports whether the computer is **online** and whether a user is **signed in**, using systemd-logind. It
registers one device with two read-only sensors.

**Status:** works. Verified on Ubuntu with GNOME on Wayland: sign-in state comes from the real logind and
reaches a Matter controller.

## Requirements

* systemd-logind on the system D-Bus (any systemd distribution). No privileges are needed: logind's session
  information is readable by every user.
* The plugin is started by the it-matters daemon (it needs the `IT_MATTERS_SOCKET` and
  `IT_MATTERS_PLUGIN_ID` environment variables the daemon sets); it exits with an explanation otherwise.

## What controllers see

One device (`--device`, named `--name`) with two units, each a Matter **Contact Sensor**:

| Capability | Matter endpoint name | Unique id | Meaning |
|---|---|---|---|
| `online` | `<name> online` | `<device>/online` | `true` while the plugin is running |
| `signed-in` | `<name> signed-in` | `<device>/signed-in` | a user is signed in at the machine (below) |

**`online` is always `true` while the plugin runs.** When the plugin stops (or the computer is off, for a
remote agent) controllers do not see `false`: they see the endpoint as **unreachable** ("No Response"). It
exists so that "this computer is up" is a sensor you can build automations on; use `Reachable` for
"is it down". See [what controllers see](../matter.md).

**`signed-in` is `true`** when logind has at least one session that is **local** (not ssh), of class
**`user`**, of a **graphical type** (`x11`, `wayland` or `mir`) and in state **`active`** or **`online`**.
It is `false` for text consoles, remote sessions, the login screen and the per-user `manager` session.
A locked screen still counts as signed in (the session is still there).

## How it stays current

It re-reads the sessions when logind reports a session being added or removed, **and** every `--poll-ms`
(default 5 seconds), because a session changing state (for example becoming active again after fast user
switching) sends no signal. A sensor is updated only when its value changes. If logind cannot be read (it is
restarting) the last values are kept, the problem is logged once, and the read is retried at the next poll.
Controllers cannot write to these sensors.

## Command line

```
it-matters-plugin-linux-system [--device <id>] [--name <name>] [--poll-ms <ms>]
```

| Flag | Default | Description |
|---|---|---|
| `--device <id>` | the host name, made into a valid id (lower case; other characters become `-`) | The device id the plugin registers: 1-64 characters of `a-z`, `0-9`, `-`, `_`. The endpoint ids are remembered by this id, so changing it makes controllers see new devices. |
| `--name <name>` | the host name | The name shown in controllers (`devices.<id>.name` in the daemon configuration overrides it). |
| `--poll-ms <ms>` | `5000` | How often to re-read the sessions, at least 100. |
| `--help`, `-h` | | Print the usage text and exit `0`. |
| `--version`, `-V` | | Print the version and exit `0`. |

Exit codes: `0` for `--help` and `--version`; `2` for a bad argument; `1` if it cannot start (no D-Bus, no
socket, refused by the daemon) or the connection to the daemon ends (the daemon then restarts it).

## Example

```yaml
plugins:
  - id: linux-system
    run:
      command: /usr/local/bin/it-matters-plugin-linux-system
      args: [--device, office-pc, --name, Office PC]
```

In the daemon's log (`RUST_LOG=info`) the plugin appears as `[linux-system]`:

```
[linux-system] connected as plugin "linux-system"; device "office-pc" (Office PC)
[linux-system] signed in: true
```

## Testing

`cargo test -p it-matters-plugin-linux-system` runs the logic and the loop against the real host with a
scripted session source. The test against the real logind is `#[ignore]`d (it needs a system bus):

```bash
cargo test -p it-matters-plugin-linux-system -- --ignored --nocapture
```

# `it-matters-plugin-linux-audio`

Reports and controls the computer's **speaker volume and mute**, and reports whether a **microphone is in
use**, using PipeWire. It registers one device: a Speaker and a microphone sensor.

**Status:** works. Verified on Ubuntu with PipeWire 1.6 and WirePlumber against a Matter controller: volume
and mute writes reach `wpctl`, and the microphone sensor follows a real capture. All write tests used a
disposable test speaker.

## Requirements

* **PipeWire with WirePlumber**, and its command line tools on `PATH`: `wpctl` (package `wireplumber`) and
  `pw-dump` (package `pipewire-bin`). It does not use PulseAudio tools. (It deliberately does not link
  `libpipewire`, so building it needs no PipeWire development headers.)
* It must run as **the desktop user, in that user's session** (it needs `XDG_RUNTIME_DIR`, normally
  `/run/user/<uid>`), so run the daemon as that user. See [bundled plugins](README.md#running-on-a-desktop-as-the-desktop-user).
* The plugin is started by the it-matters daemon (it needs `IT_MATTERS_SOCKET` and `IT_MATTERS_PLUGIN_ID`).

## What controllers see

One device (`--device`, named `--name`) with two units:

| Capability | Matter endpoint | Unique id | Writable | Meaning |
|---|---|---|---|---|
| `volume` + `mute` | **Speaker** named `<name> volume` | `<device>/volume` | yes (not with `--read-only`) | the speaker's volume (0-100) and mute |
| `mic-active` | **Occupancy Sensor** named `<name> mic-active` | `<device>/mic-active` | no | an application is using a microphone |

### Volume and mute

* `volume` is the number `wpctl get-volume` shows, times 100 (so `0.58` is `58`), rounded. PipeWire allows
  more than 100 %: **anything above 100 is reported as 100**, and writes are 0-100.
* `mute` is the speaker's mute switch. (In Matter a Speaker's On/Off means "not muted"; the host converts.)
* A write is applied with `wpctl set-volume` / `wpctl set-mute`, and the plugin **re-reads at once**, so the
  value a controller shows is what the system really did (and a poll that started before the write can never
  report the old value afterwards: the plugin reads and writes from one loop, one at a time). A failed write
  is reported to the controller as a failure, with the reason in the daemon's log.
* Changes made outside Matter (keyboard keys, the volume slider) are noticed at the next poll.

### Microphone in use (`mic-active`)

`true` while at least one application is **capturing from a microphone-type source**: a PipeWire
`Stream/Input/Audio` stream that is `running` and linked from a node of class `Audio/Source` (physical,
Bluetooth or virtual microphones). It is **`false`** for an application capturing *desktop audio* (a screen
recorder recording the speakers' monitor, which is linked from the speaker, not a source), for paused streams
and for a source that merely exists. Verified against both kinds of capture.

## Read-only mode

With `--read-only` the plugin only **reads** the speaker. Nothing a controller (or an automation) does can change
the volume or mute: the plugin declares the volume and mute as not writable, so the bridge refuses every write
itself, without asking the plugin, and as a second line of defence the plugin would refuse a write that
reached it anyway. Reading is unchanged: the volume and mute follow the desktop (within `--poll-ms`), and
the microphone sensor works as usual.

Matter has no "read-only speaker" device type, so the device still appears as a **Speaker**, and a controller's
app may still show a volume slider and mute button. Using them fails (the controller reports that the
device did not respond or the command failed) and nothing changes; the reason is in the daemon's log
(`capability "volume" is read-only`). If you only want to *see* the volume and mute in an automation, this is
what you want; if you do not want the controls at all, do not run the audio plugin (the
[system plugin](linux-system.md) reports sign-in state with no audio at all).

```yaml
plugins:
  - id: linux-audio
    run:
      command: /usr/local/bin/it-matters-plugin-linux-audio
      args: [--read-only]
```

## Choosing the speaker (`--sink`)

| Value | Meaning |
|---|---|
| `@DEFAULT_AUDIO_SINK@` (default) | whatever speaker the desktop currently uses; follows the user's choice |
| a node **name**, e.g. `alsa_output.pci-0000_c4_00.6.analog-stereo` | that speaker. The name (`node.name` in `pw-dump`, or `wpctl inspect <id>`) is looked up again on every read, so it survives PipeWire restarts, which change node ids |
| a node **id**, e.g. `60` | that node. Ids change when PipeWire restarts, so prefer a name |

`wpctl` itself understands only ids and `@DEFAULT_AUDIO_SINK@`; the plugin does the name lookup. The
microphone sensor always watches *all* microphones, whatever `--sink` is.

## How it works, and its costs

Every `--poll-ms` (default 1 s) it runs `pw-dump` (the graph, for the microphone and for name lookup) and
`wpctl get-volume`: two short-lived processes per second. It sends only values that changed. If PipeWire
cannot be read (it is restarting) the last values are kept, the problem is logged **once** (and again if the
message changes), and the read is retried at the next poll. All commands run with `LC_ALL=C`, so numbers are
parsed as `0.58` whatever the desktop's locale, and with a **1.5 second timeout**, shorter than the default
`timeout_ms` of 2000 ms for controller writes, so a hung `wpctl` is reported as a failure by the plugin
itself. The 1.5 second limit is fixed: leave the plugin's `run.timeout_ms` at 2000 or higher.

## Command line

```
it-matters-plugin-linux-audio [--device <id>] [--name <name>] [--sink <target>] [--poll-ms <ms>] [--read-only]
    [--connect <host:port> --plugin-id <id> [--token-file <file>]]
```

| Flag | Default | Description |
|---|---|---|
| `--device <id>` | `<host name>-audio` (host name made into a valid id) | The device id the plugin registers: 1-64 characters of `a-z`, `0-9`, `-`, `_`. Must differ from the system plugin's device id. The endpoint ids are remembered by this id. |
| `--name <name>` | `<host name> Audio` | The name shown in controllers (`devices.<id>.name` in the daemon configuration overrides it). |
| `--sink <target>` | `@DEFAULT_AUDIO_SINK@` | The speaker to control: `@DEFAULT_AUDIO_SINK@`, a node id, or a node name (see above). Anything else starting with `@` is rejected. |
| `--poll-ms <ms>` | `1000` | How often to read the audio state, at least 100. |
| `--read-only` | off | **Report the volume and mute but never change them.** Controllers still see the speaker (its volume and mute update as you use the desktop), but every attempt to change it fails, and the plugin never runs `wpctl set-volume` or `wpctl set-mute`. The microphone sensor is unaffected. See [read-only mode](#read-only-mode). |
| `--connect <host:port>` | none | Run as a **remote agent**: connect to the bridge's TCP listener for this agent (`plugins[].connect.listen` in the bridge's configuration) instead of the local socket. Reconnects forever, with a delay that grows from 1 to 30 seconds. See [remote agents](README.md#running-on-another-machine-as-a-remote-agent). |
| `--plugin-id <id>` | `$IT_MATTERS_PLUGIN_ID` | With `--connect`: the plugin `id` the bridge configured for this agent (required unless the environment variable is set). |
| `--token-file <file>` | none | With `--connect`: the file holding the agent's token, the same as the bridge's `token_file`. Omit it only for a bridge listener on loopback without a token. |
| `--help`, `-h` | | Print the usage text and exit `0`. |
| `--version`, `-V` | | Print the version and exit `0`. |

Exit codes: `0` for `--help` and `--version`; `2` for a bad argument; `1` if it cannot start (no socket,
refused by the daemon) or the connection to the daemon ends (the daemon then restarts it). A missing `wpctl`
or `pw-dump`, or no PipeWire, does **not** stop the plugin: it logs the problem and keeps retrying.

## Example

```yaml
plugins:
  - id: linux-audio
    run:
      command: /usr/local/bin/it-matters-plugin-linux-audio
      args: [--device, office-pc-audio, --name, Office PC Audio, --poll-ms, "500"]
```

Controlling a specific speaker by name:

```yaml
plugins:
  - id: linux-audio
    run:
      command: /usr/local/bin/it-matters-plugin-linux-audio
      args: [--sink, alsa_output.pci-0000_c4_00.6.analog-stereo]
```

As a remote agent on the desktop (the bridge configures a `connect` entry with `id: desktop-audio`):

```bash
it-matters-plugin-linux-audio --device office-pc-audio --name "Office PC Audio" \
    --connect 192.168.1.10:7551 --plugin-id desktop-audio --token-file ~/.config/it-matters/desktop-audio.token
```

In the daemon's log the plugin appears as `[linux-audio]`:

```
[linux-audio] connected as plugin "linux-audio"; device "office-pc-audio" (Office PC Audio)
[linux-audio] microphone in use: true
[linux-audio] cannot read the audio state (keeping the last values, will retry): cannot run `wpctl`: ...
```

## Troubleshooting

* `cannot run `wpctl`: ... (is it installed?)`: install `wireplumber` / `pipewire-bin`, and check `wpctl status`
  works in the same terminal that runs the daemon.
* `wpctl` works in your terminal but not from the daemon: the daemon runs as another user or without the
  user's `XDG_RUNTIME_DIR`. Run it as the desktop user.
* `no PipeWire node named "..."`: the `--sink` name does not exist; list names with `pw-dump | grep node.name`.
* The controller's volume lags the keyboard volume keys by up to `--poll-ms`.

## Testing

`cargo test -p it-matters-plugin-linux-audio` runs the parsing, the command construction (with a fake
command runner) and the loop against the real host with a fake audio system, including the ordering of a
write and a poll. Two tests use the real PipeWire and are `#[ignore]`d:

```bash
cargo test -p it-matters-plugin-linux-audio --test run -- --ignored --test-threads=1
```

* the first creates a **temporary null speaker** (`it-matters-test-sink`, lowest priority, so it can never
  become the default), reads and writes only that, and **asserts that your real default speaker was not
  touched**; the speaker disappears when the test ends;
* the second **records a few seconds of the default microphone to `/dev/null`** (the microphone indicator
  may light) and of the speaker's monitor, to check the two are told apart.

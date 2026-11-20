# Plugins and devices

Plugins are separate processes. Locally running plugins connect to the Unix socket in `ipc.socket`; a plugin
listed with `run` is **started and supervised by the daemon**, one listed with `connect` is a remote agent
that connects over TCP. **A plugin only connects if its id is listed here** (anything else is refused with
`plugin ... is not configured`): a plugin cannot choose its own executable or claim an id that is not in
the file.

Ready-made plugins you can list here are described in [bundled plugins](../plugins/README.md).

A plugin listed with `connect` is a **remote agent**: a program on another machine (for example the Linux
plugins on your desktop) that connects to a TCP port of the bridge. Nobody starts it for the bridge, so it runs
and restarts itself on its own machine. See [remote agents](#remote-agents) below.

## How the daemon runs a `run` plugin

* **Start.** After the plugin socket is bound the daemon starts every `run` plugin, each as
  `command args...`. `command` is looked up in `PATH` if it has no `/`; a relative path is relative to the
  daemon's working directory (which is `/` under systemd), so **prefer absolute paths**.
* **User and environment.** The plugin runs as the daemon's user and inherits the daemon's environment,
  plus two variables: `IT_MATTERS_SOCKET` (the absolute path of `ipc.socket`) and `IT_MATTERS_PLUGIN_ID`
  (the plugin's `id`). A plugin can read these instead of hard-coding either. Standard input is closed.
* **Process group.** Each plugin gets its own process group, so the daemon can stop everything the plugin
  started.
* **Output.** Standard output is logged at `info` and standard error at `warn`, one log line per output
  line, prefixed `[id]`, with log target `plugin` (for example `RUST_LOG=plugin=debug`). Lines longer than
  8 KiB are cut and end with ` [line cut]`.
* **Restarts.** A plugin that exits **for any reason, including a clean exit with status 0**, is started
  again after a delay: 1 s, then doubling (2, 4, 8, ...) up to 60 s. A run that lasted at least 30 s resets
  the delay to 1 s. A command that cannot be started (not found, not executable) is retried the same way
  and logs `cannot start: ...`; it does not stop the daemon. These timings are fixed, not configurable.
  Whenever a plugin exits, everything left in its process group is killed (SIGKILL), so a crashed plugin
  cannot leave helpers running beside its replacement.
* **Reachability.** While a plugin is down its devices are `Reachable = false`; when the restarted plugin
  registers again they return on the **same endpoints** with their last known values.
* **Shutdown.** On `SIGINT`/`SIGTERM` the daemon sends SIGTERM to each plugin's process group, waits up to
  5 s, then sends SIGKILL, before removing the socket and exiting.
* **If the daemon is killed** (`kill -9`, a crash, the OOM killer) it cannot clean up, but the kernel sends
  SIGTERM to each plugin's *main* process, so plugins are not left orphaned. Helper processes a plugin
  started itself are not signalled in that case. The stale socket is replaced on the next start.
* **Plugins started by hand.** A plugin you start yourself can connect with a listed `id` too, but only one
  connection per id is accepted: if a hand-started copy holds the id, the supervised copy is refused
  (`already connected`), exits, and is restarted with growing delays. Run each id one way only.

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
| `plugins[].run` | map | none | startup only | A local plugin that the daemon starts and supervises (see above). |
| `plugins[].run.command` | path | required | startup only | Executable to start; must not be empty. Looked up in `PATH` if it contains no `/`. Prefer an absolute path. |
| `plugins[].run.args` | list of strings | `[]` | startup only | Arguments passed to `command`, one list entry per argument (no shell is involved, so no quoting or expansion). |
| `plugins[].run.timeout_ms` | integer | `2000` | startup only | How long the plugin has to answer each write a controller makes (`set`); after that the controller gets a failure. Must be greater than 0. (The handshake timeout is a fixed 5 s.) |
| `plugins[].connect` | map | none | startup only | A TCP listener for one remote plugin agent (see [remote agents](#remote-agents)). |
| `plugins[].connect.listen` | string | required | startup only | `ip:port` to listen on, e.g. `192.168.1.10:7550`. A hostname is not accepted. Each address may be used once, and the daemon refuses to start if the port is taken. Prefer the address of your LAN or VPN interface to `0.0.0.0`. |
| `plugins[].connect.token_file` | path | none | startup only | File holding the agent's token: the first line, at least 32 and at most 1024 characters. The agent proves it knows it; the token itself never crosses the network. **Required unless `listen` is a loopback address.** The daemon refuses to start if the file is missing, empty or too short, and warns if other users can read it. Use one token per agent. See [tokens](#tokens). |
| `devices` | map | `{}` | startup only | Overrides keyed by device id (the id a plugin registered). |
| `devices.<id>` | map | n/a | startup only | Overrides for one device. |
| `devices.<id>.name` | string | none | startup only | Name shown in controllers instead of the plugin's own device name. For a device made of several units the unit's id is appended (`Office PC online`). Must not be empty. |

## Remote agents

A remote agent lets a computer other than the bridge's host report its state: the bridge can run on an
always-on machine (a Raspberry Pi, a server) while each desktop runs the bundled
[plugins](../plugins/README.md) as agents. This is also what makes "online" meaningful: when a desktop
loses power or the network, its devices become **unreachable** within about 30 seconds (the bridge pings every
10 seconds and drops an agent that is silent for 30).

```yaml
plugins:
  - id: desktop
    connect:
      listen: 192.168.1.10:7550            # an address of the bridge machine; the agent connects here
      token_file: /etc/it-matters/desktop.token
```

* **One listener per agent.** A listener serves exactly the plugin it is configured for: a connection that
  claims any other id is refused, even with the right token. Give each agent its own `id`, port and token.
* **Local and remote ids do not mix.** An id listed with `connect` is refused on the local Unix socket, and
  an id listed with `run` is refused on a TCP listener.
* **Reconnecting.** When an agent reconnects (its computer rebooted, a laptop woke up, the network blipped)
  while the bridge still holds the old connection, the **new authenticated connection replaces the old one**
  and the devices come back on the same endpoints. Without a token (loopback only) there is no way to tell
  the new connection from an intruder, so a second connection is refused (`already connected`) while the first
  lives.
* **Connections.** At most 16 connections per listener are served at once; an agent must complete the
  handshake within 5 seconds. Agents are plugins like any other: the bridge pings them and they answer.

### Tokens

Create a token and give the **same file contents** to the bridge (`token_file`) and to the agent
(`--token-file`):

```bash
openssl rand -hex 32 > /etc/it-matters/desktop.token       # 64 hex characters
chmod 600 /etc/it-matters/desktop.token
# or, without openssl:
head -c 32 /dev/urandom | xxd -p -c 64 > /etc/it-matters/desktop.token
```

Copy the file to the agent machine over a safe channel (for example `scp`). The bridge reads it once at
start; **restart the daemon after changing it**. `it-matters check` also reads the token files, so run it
after creating them. Token contents are never logged or shown in error messages.

### What this protects, and what it does not

* **Protected:** the token is never sent. The bridge sends a fresh random challenge and the agent answers with
  an HMAC-SHA256 over it, so recording a login does not let anyone log in later, and a proof for one plugin id
  is useless for another. Failed logins are answered identically (`authentication failed`, whether the id or
  the token was wrong) after a short delay and are logged with the peer's address.
* **Not protected: the connection itself.** Nothing is encrypted or integrity-protected. Anyone who can
  intercept the traffic between the agent and the bridge (on the same Wi-Fi, for example) can **read** what
  the agent reports and **hijack a live session** (and so impersonate that one plugin's devices). The impact
  is limited to the devices of that plugin, but you should still use a network you trust, or run the
  connection through a tunnel (WireGuard/netbird, or `ssh -L`) and listen on the tunnel's address. TLS is not
  implemented.
* **Loopback without a token.** A listener on `127.0.0.1` may omit `token_file`, but then *any program on that
  machine* can connect as the plugin. A local Unix socket is protected by file permissions; a loopback port is
  not. Set a token anyway.
* **Firewall.** Open the listener's TCP port to the agent's machine only.

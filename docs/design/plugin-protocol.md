# Plugin protocol (v1)

The complete reference for how a plugin talks to the it-matters host: the wire format, every message, the rules
the host enforces, the error messages, the timings and two full example sessions. It is the contract for plugins
in **any language**. (The Rust SDK, [`plugins/sdk.md`](../plugins/sdk.md), is one implementation; so is
[`examples/plugins/demo.py`](../../examples/plugins/demo.py).) The types are in
`crates/it-matters-model/src/protocol.rs`; how the host uses them is in [`architecture.md`](architecture.md).

## Contents

1. [Transports](#1-transports)
2. [Framing](#2-framing)
3. [Sessions: handshake, liveness, ending](#3-sessions-handshake-liveness-ending)
4. [Devices and capabilities](#4-devices-and-capabilities)
5. [Messages, plugin to host](#5-messages-plugin-to-host)
6. [Messages, host to plugin](#6-messages-host-to-plugin)
7. [Remote agents (TCP) and authentication](#7-remote-agents-tcp-and-authentication)
8. [Lifecycle rules](#8-lifecycle-rules)
9. [Errors](#9-errors)
10. [Timings and limits](#10-timings-and-limits)
11. [Versioning and compatibility](#11-versioning-and-compatibility)
12. [Example sessions](#12-example-sessions)

## 1. Transports

| Transport | Used by | Where | Authentication |
|---|---|---|---|
| **Unix socket** | plugins on the bridge's machine, usually started by the daemon | `ipc.socket` (default `/run/it-matters/plugins.sock`), mode `0660` | none: the socket's file permissions are the trust boundary |
| **TCP** | **remote agents** on other machines | `plugins[].connect.listen`, one listener per agent | a shared token, proved with a challenge (section 7). **Not encrypted** |

The messages are identical on both; only the start of the handshake differs (section 7). A plugin started by
the daemon (`run`) finds the socket in `IT_MATTERS_SOCKET` and its id in `IT_MATTERS_PLUGIN_ID`.

## 2. Framing

* One **JSON object per line**, UTF-8, ended by `\n` (`\r\n` is accepted). At most **65536 bytes** per line,
  not counting the newline.
* Every object has a string field `"type"` that selects the message; the other fields depend on the type.
* A blank line, text that is not UTF-8 or JSON, a line over the limit, a missing required field, a field of
  the wrong type, or an **unknown `type`** is a protocol violation: the receiver sends `error` (the host does;
  see section 9) and **closes the connection**.
* **Unknown extra fields are ignored**, so a newer peer may add optional fields within the same protocol
  version (section 11).
* Both directions are independent streams; there is no request/response pairing except `set` and
  `set_result` (by `req_id`).
* JSON numbers for levels must be **integers**. A fraction such as `30.0` or `0.5` is not a valid value and is
  a protocol violation.

## 3. Sessions: handshake, liveness, ending

```text
plugin                                       host
  |  (TCP only)  <------ challenge ----------- |
  | ---------------- hello ------------------> |  within 5 s
  | <--------------- welcome ----------------- |
  | ---- register / update / unregister -----> |
  | <---- set  ........  set_result ---------> |  controller writes
  | <------ ping .......... pong ------------> |  either direction
```

* **Handshake.** The first message from the plugin must be `hello`, within **5 seconds** of the connection
  (of the `challenge`, on TCP). The host answers `welcome` or an `error` followed by closing the connection.
  The checks, in order: the protocol version (`proto`), then on the Unix socket that the plugin id is configured
  and is not a remote agent, or on TCP the authentication (section 7), then that no other session of that
  plugin is live (Unix socket) / replacing it (TCP).
* **Registration comes after `welcome`.** Nothing is registered by connecting.
* **Liveness.** The host sends `ping` every **10 seconds** and closes a connection from which it has received
  *nothing* for **30 seconds**. Any message from the plugin counts as activity, so a plugin that sends updates
  regularly needs no `pong`, but a plugin must always answer a `ping` with `pong` (the SDK does this itself).
  Either side may send `ping`; the peer answers `pong`.
* **Ending.** When the connection ends (the plugin exits, the network drops, the host closes it, the idle
  timeout fires) the host marks all of the plugin's devices **unreachable** (Matter `Reachable = false`) while
  keeping their endpoints. A plugin should then exit (if the daemon started it, the daemon restarts it) or, as a
  remote agent, reconnect with a growing delay, and **start the session from scratch**: `hello`, then `register`
  every device and `update` every value again.
* **One live session per plugin id.** On the Unix socket a second connection is refused (`already connected`)
  while the first lives. On TCP an *authenticated* reconnect **replaces** the old session (the old connection is
  closed and anything it was asked to do fails with `the plugin reconnected`); a TCP listener without a token
  refuses the second connection like the Unix socket does.

## 4. Devices and capabilities

A plugin describes **devices**; the host maps them onto Matter. A plugin never sees Matter.

**Device** (the `device` field of `register`):

| Field | Type | Required | Rules |
|---|---|---|---|
| `id` | string | yes | 1-64 characters of `a-z`, `0-9`, `-`, `_`. Unique in the whole bridge: it belongs to the plugin that first registered it. Endpoint ids are remembered by it. |
| `name` | string | yes | Not empty (after trimming). Shown in controllers; the bridge's `devices.<id>.name` can override it. |
| `capabilities` | array | yes | At least one. |

**Capability** (an element of `capabilities`):

| Field | Type | Required | Rules |
|---|---|---|---|
| `id` | string | yes | Same character rules as a device id; unique within the device. |
| `type` | `"boolean"` or `"level"` | yes | The shape of the value. |
| `min`, `max` | integers | **required for `level`**, absent for `boolean` | `min` must be less than `max`. |
| `writable` | boolean | no, default `false` | Whether controllers may change it; the host then sends `set`. |
| `role` | `"contact"`, `"occupancy"` or `"mute"` | no | A hint that steers the Matter mapping (below). |

**Values** are plain JSON: `true`/`false` for a `boolean`, an integer in `min..=max` for a `level`. `update`
and `set` carry them in a field called `value`; a value of the wrong shape or outside the range is rejected.

### Mapping to Matter

Each device is split into **units**; each unit becomes one Matter endpoint. Anything not listed is **rejected
at `register`** with an `error` that names the capability: nothing is dropped silently.

| Capabilities of the device | Becomes |
|---|---|
| a read-only `boolean` with role `contact` or none | a **Contact Sensor** (unit id = the capability id) |
| a read-only `boolean` with role `occupancy` | an **Occupancy Sensor** |
| a writable `boolean` with no role | an **On/Off Plug-in Unit** |
| exactly one writable `level` **and** exactly one writable `boolean` with role `mute` | a **Speaker** (unit id = the level's id) |

Rejected: a `level` without a `mute` (and the reverse); more than one `level` or more than one `mute`; a
read-only level or mute; a *writable* boolean with role `contact` or `occupancy` (sensors are read-only); a
device that maps to no unit. A device may contain a speaker **and** any number of sensors or plugs.

How values map: a Speaker's volume is scaled from `min..=max` to Matter's `0..=254` (rounded; ranges up to 254
wide round-trip exactly); a Speaker's Matter On/Off means *not muted*, which the host inverts, so a plugin
only ever sees `mute`. Endpoint names are the device name, with the unit id appended when the device has
several units (`Office PC online`). What controllers see in full: [`../matter.md`](../matter.md).

## 5. Messages, plugin to host

### `hello`

The first message. `{"type":"hello","plugin":"linux-system","version":"0.1.0","proto":1}`

| Field | Type | Required | Meaning |
|---|---|---|---|
| `plugin` | string | yes | The plugin id as listed in the bridge's configuration (`plugins[].id`). |
| `version` | string | yes | The plugin's own version; informational only, not checked. |
| `proto` | integer | yes | The protocol version the plugin speaks: **must be `1`**, else `error` and close. |
| `proof` | string | TCP only | The authentication proof (section 7). Ignored on the Unix socket. |

Sending `hello` again after `welcome` is an error (`already said hello`) and closes the connection.

### `register`

Registers a device, or re-registers it (after a reconnect, or to change it).
`{"type":"register","device":{"id":"desk-lamp","name":"Desk Lamp","capabilities":[{"id":"power","type":"boolean","writable":true}]}}`

The device follows section 4. On success the host creates (or re-uses) one endpoint per unit. On failure the
host answers with an `error` and the connection **stays open**; nothing changed. See section 8 for what
re-registering does.

### `unregister`

Removes a device the plugin registered. `{"type":"unregister","device":"desk-lamp"}`

The device's endpoints disappear from the controllers. Their ids stay reserved. Unknown devices, or devices of
another plugin, give an `error`.

### `update`

A capability's value changed (or is being reported for the first time).
`{"type":"update","device":"desk-lamp","capability":"power","value":true}`

| Field | Type | Required | Meaning |
|---|---|---|---|
| `device` | string | yes | The device id. |
| `capability` | string | yes | The capability id. |
| `value` | boolean or integer | yes | Section 4. |

**Send every current value right after `register`**: until then the host reports defaults (`false`, or the
level's `min`). Send only changes afterwards (the host drops an `update` that repeats the stored value, but
there is no reason to send it). Invalid updates (unknown device or capability, not your device, wrong shape,
out of range) give an `error` and the connection stays open. Updates for a device are applied in the order
sent.

### `set_result`

The answer to a host `set`. `{"type":"set_result","req_id":7,"ok":true}` or
`{"type":"set_result","req_id":7,"ok":false,"error":"no such sink"}`

| Field | Type | Required | Meaning |
|---|---|---|---|
| `req_id` | integer | yes | The `req_id` of the `set` being answered. |
| `ok` | boolean | yes | Whether the change was applied. |
| `error` | string | no | Why not, shown in the host's log; the controller only learns that the write failed. Default text: `the plugin refused the change`. |

An answer for an unknown, expired or already answered `req_id`, or from a plugin the request was not sent to,
is ignored.

### `ping`

`{"type":"ping"}`: no fields. Asks the host to answer `pong`; the plugin may send it any time. Section 3.

### `pong`

`{"type":"pong"}`: no fields. The answer to a host `ping`; send it promptly (the SDK does). Section 3.

## 6. Messages, host to plugin

### `challenge`

**TCP only**, sent first, unasked: `{"type":"challenge","nonce":"<32 hex characters>"}`. Section 7.

### `welcome`

`{"type":"welcome","proto":1}`: the handshake succeeded. `proto` is the version the host speaks.

### `set`

A controller wrote a writable capability.
`{"type":"set","req_id":7,"device":"desk-lamp","capability":"power","value":false}`

| Field | Type | Meaning |
|---|---|---|
| `req_id` | integer | Identifies the request; echo it in `set_result`. Unique per host run. |
| `device`, `capability` | string | What to change: always a `writable` capability of the plugin's own device. |
| `value` | boolean or integer | The new value **in the plugin's terms**: a Speaker's level already scaled back to `min..=max`, and mute already un-inverted. |

Apply it, then answer with `set_result` within the plugin's `timeout_ms` (`plugins[].run.timeout_ms`, default
**2000 ms**; the default also applies to remote agents). If the plugin answers `ok`, the controller sees
success and the new value is reported at once; if it answers `ok:false`, does not answer in time, or
disconnects, the controller sees a failure and nothing changes. Several `set`s can be outstanding at once.
Writes to devices that are unreachable are refused by the host and never sent.

### `error`

`{"type":"error","message":"authentication failed"}`: the host rejected something. Handshake errors, protocol
violations and oversized frames are followed by the host **closing the connection**; errors about a `register`,
`update` or `unregister` leave it open. The message is for humans and logs; do not parse it. Section 9 lists them.

### `ping`

`{"type":"ping"}`: no fields. The plugin answers `pong`. Sent every 10 seconds. Section 3.

### `pong`

`{"type":"pong"}`: no fields. The answer to a plugin `ping`. Section 3.

## 7. Remote agents (TCP) and authentication

A plugin on another machine connects to the TCP listener the bridge configured for it
(`plugins[].connect.listen`). The messages are the same; the handshake differs:

1. **The host speaks first.** As soon as the connection opens it sends a fresh random nonce:
   `{"type":"challenge","nonce":"<32 hex characters>"}` (16 random bytes from the operating system).
2. The agent's `hello` carries a **proof** that it knows the agent's token:
   `{"type":"hello","plugin":"desktop","version":"1","proto":1,"proof":"<64 hex characters>"}`.

```text
proof = hex( HMAC-SHA256( key = token bytes,
                          msg = "it-matters-agent-v1" 0x00 nonce 0x00 plugin_id ) )
```

* `token` is the **first line of the token file**, trailing whitespace removed, as UTF-8 bytes (the file is
  32-1024 characters; create one with `openssl rand -hex 32`). **The token itself is never sent.**
* `nonce` is the challenge's text exactly as received; `plugin_id` is the `plugin` field of the `hello`.
* The result is **lower-case hex** (the host accepts either case). In Python:

  ```python
  proof = hmac.new(token.encode(), b"it-matters-agent-v1\0" + nonce.encode() + b"\0" + plugin_id.encode(),
                   hashlib.sha256).hexdigest()
  ```

  The reference value `feb5c3b891baf76cef208f5821094b670d42662d044181dd9e4637ef1514de87` is the proof for token
  `0123456789abcdef0123456789abcdef`, nonce `00112233445566778899aabbccddeeff` and plugin `desktop-agent`; use it
  to test an implementation.
* A fresh nonce per connection makes a recorded proof useless on another connection, and binding the plugin id
  stops a proof for one plugin being used for another. The host compares in constant time.
* **One listener serves one plugin.** A `hello` whose `plugin` is not that listener's plugin fails, even with a
  valid proof for it. Ids configured with `run` cannot connect on a TCP listener, and ids configured with
  `connect` are refused on the Unix socket (`plugin "x" is a remote agent: it connects to its own TCP listener`).
* **Every failure looks the same:** a wrong token, a missing or malformed proof, a wrong or unknown plugin id
  all get `{"type":"error","message":"authentication failed"}` after a short delay (500 ms by default) and the
  connection is closed. The host logs the peer address and the claimed id.
* A listener **without a token** (allowed only on a loopback address) sends the challenge too but ignores the
  proof; any program on that machine can connect as the plugin.
* **No encryption or integrity protection** after the handshake: someone who can intercept a live connection
  can read it and take it over (and so impersonate that one plugin's devices). Use a trusted network or a
  tunnel. See [the configuration reference](../config/plugins.md#what-this-protects-and-what-it-does-not).

## 8. Lifecycle rules

* **Ownership.** A device id belongs to the plugin that first registered it, until that device is
  unregistered. Another plugin registering, updating or unregistering it gets an `error`.
* **Disconnect.** All the plugin's devices become unreachable; their endpoints, names and last values are kept.
* **Re-registering** (typically after a reconnect) with the same shape makes the devices reachable again on
  the **same endpoints**, with their last known values until updated. If a unit's shape changed (for example
  its role, or it is now a different Matter device) it gets a **new** endpoint and the old one is removed; if a
  capability was dropped, its endpoint is removed. Ids are persisted and **never reused** for something else.
  A value that no longer fits its capability (for example a smaller range) is forgotten.
* **Unregister** removes the endpoints; registering the same unit again later gets its old endpoint id back.
* **Ordering.** Messages from one connection are processed in order. Across plugins there is one total order.
* **Back-pressure.** The host reads a connection as fast as it can apply its messages; if the Matter side is
  slow the host slows the plugin down by not reading. There is no explicit rate limit.

## 9. Errors

Messages the host sends in `error`, grouped by what happens next (`{x}` is filled in):

**The connection is closed afterwards**

| Message | Cause |
|---|---|
| `handshake timed out` | No `hello` within 5 seconds. |
| `the first message must be hello` | The first message was something else. |
| `unsupported protocol {n}; this host speaks 1` | `proto` is not 1. |
| `plugin "{id}" is not configured` | Unix socket: the id is not listed under `plugins`. |
| `plugin "{id}" is a remote agent: it connects to its own TCP listener, not this socket` | Unix socket: the id is configured with `connect`. |
| `plugin "{id}" is already connected` | Unix socket (or a TCP listener without a token): that plugin is live. |
| `authentication failed` | TCP: any login failure (section 7). |
| `already said hello` | A second `hello`. |
| `frame too long` | A line over 65536 bytes. |
| `invalid frame: {reason}`, `empty frame`, `frame is not valid UTF-8` | Section 2. |

**The connection stays open**

| Message (examples) | Cause |
|---|---|
| `device "{id}": id must be 1-64 characters of a-z, 0-9, '-' or '_'` | Invalid device or capability id. |
| `device "{id}": name must not be empty`, `needs at least one capability`, `duplicate capability id "{c}"`, `capability "{c}": min must be less than max` | Invalid device (section 4). |
| `device "{id}": level capability "{c}" needs a boolean with role "mute" to form a speaker` (and similar) | Unmappable device (section 4). |
| `device "{id}" is already registered by plugin "{p}"` | Another plugin owns the id. |
| `device "{id}" is not registered`, `device "{id}" belongs to plugin "{p}"` | `update`/`unregister` for a device that is not yours. |
| `device "{id}" has no capability "{c}"` | Unknown capability. |
| `value {v} is not valid for "{id}"/"{c}"` | Wrong shape or out of range. |

## 10. Timings and limits

| What | Value | Configurable |
|---|---|---|
| Handshake (`hello`) | 5 s | no |
| Host `ping` interval | 10 s | no |
| Idle timeout (nothing received) | 30 s | no |
| Answer to a `set` | `plugins[].run.timeout_ms`, default 2000 ms | yes |
| Authentication failure delay | 500 ms | no |
| Line length | 65536 bytes | no |
| TCP connections per agent listener | 16 | no |
| Remote agent reconnect delay (Rust SDK) | 1 s doubling to 30 s | no |
| Plugin restart delay (daemon-started plugins) | 1 s doubling to 60 s, reset after a 30 s run | no |

A computer that loses power or its network is therefore shown unreachable within about **30 seconds**.

## 11. Versioning and compatibility

The protocol version is the integer `proto` (currently **1**), exchanged in `hello` and `welcome`. The host
accepts only its own version. Within a version the protocol may grow in compatible ways: new optional fields
(which receivers ignore) and new message types **only if** the receiver is told by a newer `proto` first. A
change that breaks existing plugins bumps `proto`. A plugin must not send a message type it was not told about,
and the host closes a connection that sends an unknown type.

## 12. Example sessions

**A plugin started by the daemon** (`>` plugin to host, `<` host to plugin; a speaker and a sensor):

```yml
> {"type":"hello","plugin":"demo","version":"0.1.0","proto":1}
< {"type":"welcome","proto":1}
> {"type":"register","device":{"id":"desktop-audio","name":"Desktop Audio","capabilities":[{"id":"volume","type":"level","min":0,"max":100,"writable":true},{"id":"mute","type":"boolean","writable":true,"role":"mute"}]}}
> {"type":"register","device":{"id":"pc","name":"Office PC","capabilities":[{"id":"mic-active","type":"boolean","role":"occupancy"}]}}
> {"type":"update","device":"desktop-audio","capability":"volume","value":30}
> {"type":"update","device":"desktop-audio","capability":"mute","value":false}
> {"type":"update","device":"pc","capability":"mic-active","value":false}
< {"type":"ping"}
> {"type":"pong"}
< {"type":"set","req_id":1,"device":"desktop-audio","capability":"volume","value":50}
> {"type":"set_result","req_id":1,"ok":true}
> {"type":"update","device":"desktop-audio","capability":"volume","value":50}
> {"type":"update","device":"pc","capability":"mic-active","value":true}
```

**A remote agent over TCP, with a token** (the proof is `HMAC-SHA256` as in section 7):

```yml
< {"type":"challenge","nonce":"00112233445566778899aabbccddeeff"}
> {"type":"hello","plugin":"desktop-agent","version":"0.1.0","proto":1,"proof":"feb5c3b891baf76cef208f5821094b670d42662d044181dd9e4637ef1514de87"}
< {"type":"welcome","proto":1}
> {"type":"register","device":{"id":"office-pc","name":"Office PC","capabilities":[{"id":"online","type":"boolean"}]}}
> {"type":"update","device":"office-pc","capability":"online","value":true}
```

**A refused login** (wrong token, wrong plugin id, or no proof: always the same):

```yml
< {"type":"challenge","nonce":"8b1f0c52a0c14d5e9a3b7d6e4f201a99"}
> {"type":"hello","plugin":"desktop-agent","version":"0.1.0","proto":1}
< {"type":"error","message":"authentication failed"}
(the host closes the connection)
```

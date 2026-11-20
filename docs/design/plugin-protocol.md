# Plugin protocol (v1)

Plugins are separate processes. They connect to the host's Unix socket (`ipc.socket`, mode `0660`) and exchange
**one JSON object per line** in each direction, UTF-8, `\n`-terminated, at most 65536 bytes per line.
Every object has a `"type"`. The types are defined in `crates/it-matters-model/src/protocol.rs`.

## Connection

1. Connect, then send `hello` within 5 seconds:
   `{"type":"hello","plugin":"linux-system","version":"0.1.0","proto":1}`.
   `plugin` must be an id listed under `plugins` in the host's config, otherwise the host sends
   `{"type":"error","message":...}` and closes the connection. `token` is for remote (TCP)
   connections, which are not implemented yet (the field is accepted and ignored on the Unix socket).
2. The host answers `{"type":"welcome","proto":1}`.
3. Only one connection per plugin id is accepted at a time. A second one is refused while the first
   is alive, so a reconnecting plugin should retry with backoff.

## Plugin to host

| Message | Meaning |
|---|---|
| `register` `{device}` | Registers a device, or re-registers it after a reconnect. A device id belongs to the plugin that first registered it. |
| `unregister` `{device}` | Removes a device the plugin registered. |
| `update` `{device, capability, value}` | A capability's value changed. Send the current values right after `register`: until then the host reports defaults (false, or the level minimum). |
| `set_result` `{req_id, ok, error?}` | The answer to a host `set`. |
| `ping` / `pong` | Heartbeat. |

A device is `{"id","name","capabilities":[{"id","type":"boolean"|"level","writable","role","min","max"}]}`.
Ids are 1-64 characters of `a-z`, `0-9`, `-`, `_`.

### How devices map to Matter

| Capabilities | Matter device |
|---|---|
| read-only boolean (role `contact` or none) | Contact Sensor |
| read-only boolean, role `occupancy` | Occupancy Sensor |
| writable boolean, no role | On/Off Plug-in Unit |
| writable level + writable boolean with role `mute` | Speaker (Level Control + On/Off) |

Anything else is rejected with an `error` message that names the capability. Notes:

* A Speaker's On/Off means *not muted*: the host inverts it, so a plugin only ever sees `mute`.
* Levels are scaled between the capability's `min..=max` and Matter's `0..=254`. Ranges up to 254 wide
  round-trip exactly.
* Each unit is one bridged endpoint. A device with several units (for example a speaker and two
  sensors) appears as several endpoints named `<device name> <unit id>`.
* If a unit's shape changes on re-registration (for example `occupancy` becomes `contact`) it gets a
  **new** endpoint id; ids are persisted and never reused.

## Host to plugin

| Message | Meaning |
|---|---|
| `welcome` `{proto}` | The handshake succeeded. |
| `set` `{req_id, device, capability, value}` | A controller wrote a writable capability. Answer with `set_result` within the plugin's `set_timeout` (2000 ms by default), otherwise the controller gets an error and a late answer is ignored. |
| `error` `{message}` | The host rejected something. After a handshake error, or a malformed or oversized frame, the host closes the connection; other errors (for example an invalid device) leave it open. |
| `ping` / `pong` | Heartbeat. |

## Liveness

The host pings every 10 seconds and closes a connection that sent nothing for 30 seconds; any message
counts as activity, so a busy plugin need not answer pings. When a connection ends, the plugin's
devices become **unreachable** (Matter `Reachable = false`) but keep their endpoints; registering
again with the same shape makes them reachable again.

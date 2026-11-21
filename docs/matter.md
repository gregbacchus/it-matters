# What controllers see

How plugin devices appear in Matter, which attributes and commands are supported, and what has (and has
not) been verified. The mapping rules from plugin capabilities to these devices are in
[`design/plugin-protocol.md`](design/plugin-protocol.md).

## The node

| Endpoint | Device type | Meaning |
|---|---|---|
| 0 | Root Node | The bridge itself: commissioning, networking, access control. Its device name is `bridge.name`. |
| 1 | Aggregator | Lists every bridged endpoint in its Descriptor parts list. It changes as plugins register and unregister, **without restarting the bridge and without re-pairing**. |
| 2 and up | one per *unit* of a plugin device | See the table below. The id is stable and never reused (see [endpoint ids](config/matter.md#endpoint-ids)). |

Every bridged endpoint is also a *Bridged Node* and carries **Bridged Device Basic Information**:

| Attribute | Value |
|---|---|
| `NodeLabel` | The unit's name: the device name (or `devices.<id>.name`), with the unit id appended when the device has more than one unit (`Office PC online`). At most 32 characters. Read-only: controllers cannot rename it. |
| `UniqueID` | `<device id>/<unit id>`, at most 32 characters. |
| `Reachable` | `true` while the owning plugin is connected, `false` after it disconnects or stops answering heartbeats. |

## Units

| Unit (from the plugin's capabilities) | Matter device type | Clusters and what they report |
|---|---|---|
| read-only boolean, role `contact` or none | Contact Sensor (0x0015) | Boolean State: `StateValue` = the plugin's value. |
| read-only boolean, role `occupancy` | Occupancy Sensor (0x0107) | Occupancy Sensing: `Occupancy` bit 0 = the plugin's value; sensor type *Physical Contact*. |
| writable boolean, no role | On/Off Plug-in Unit (0x010A) | On/Off: `OnOff` = the plugin's value; commands On, Off, Toggle. |
| writable level + writable boolean with role `mute` | Speaker (0x0022) | On/Off: `OnOff` is **On when not muted**; Level Control: `CurrentLevel` (below). |

### Speaker details

* `CurrentLevel` runs from 0 to 254 (`MinLevel` 0, `MaxLevel` 254) and is scaled from the plugin's
  `min..=max` (rounding to nearest). Ranges up to 254 wide survive the round trip exactly.
* On/Off commands set the plugin's `mute` (Off = muted, On = not muted); Toggle flips it.
* Level Control commands are applied **immediately**; transition times are accepted and ignored:
  `MoveToLevel` and `MoveToLevelWithOnOff` set the level; `Move` and `MoveWithOnOff` jump to the maximum
  (up) or 0 (down); `Step` and `StepWithOnOff` change the level by the step size, clamped to 0..254;
  `Stop` and `StopWithOnOff` do nothing. The `...WithOnOff` variants do not change the mute state.
* Attributes `Options` and `OnLevel` can be written but are not stored.

### Writes from a controller

A controller write is forwarded to the owning plugin as a `set` and the controller waits for the plugin's
`set_result` (up to the plugin's `timeout_ms`). If the plugin accepts, the controller gets *Success* and
the new value is reported at once; if it refuses, does not answer in time, or is not connected, the
controller gets *Failure* (the reason is in the daemon's log). Sensors accept no writes.

## What has been verified

Against the **matter.js shell** controller (`@matter/nodejs-shell` 0.18.0) on Linux with avahi, see
[`testing/e2e.md`](testing/e2e.md):

* commissioning over IP with the printed pairing code; devices, names, unique ids and parts list;
* reading speaker level and mute, sensor and occupancy values, and `Reachable`;
* controller writes (`MoveToLevel`, `Off`, `Toggle`) reaching the plugin, with the level scaled and the
  mute polarity inverted;
* `Reachable` turning `false` on plugin disconnect, writes then failing, and recovery on reconnect with
  the same endpoint ids;
* devices appearing while a controller is connected (parts list `[]` to `[2,3,4]`);
* **remote agents over TCP** (the bridge's real LAN address; a Rust and a Python agent): wrong tokens and ids are refused identically; an agent frozen with `SIGSTOP` (a powered-off computer) shows `Reachable = false` after about 30 seconds, and after it resumes it reconnects by itself and its devices return on the same endpoints.

**Not verified:** Home Assistant, Apple Home and Google Home (the bridge uses test attestation
certificates, so these may warn about an uncertified device or refuse it); subscriptions/reports as seen by
those controllers; Occupancy and Boolean State reports after a *live* change as seen by a subscribed
controller; commissioning more than one fabric; a `Reachable` event (only the attribute is updated);
endpoints disappearing (unregister) while a controller is connected (covered by unit tests only).

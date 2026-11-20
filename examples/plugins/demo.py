#!/usr/bin/env python3
"""A demonstration it-matters plugin, using only the Python standard library.

It registers two devices and keeps them moving so you can watch them in a Matter controller:

  * "desktop-audio": a speaker (volume 0-100 and a mute switch, both writable by controllers)
  * "pc": two read-only sensors, "online" (always true) and "mic-active" (flips every 20 seconds)

Usage:
    python3 examples/plugins/demo.py [socket] [plugin-id]
    python3 examples/plugins/demo.py --connect HOST:PORT --plugin-id ID [--token-file FILE]

`socket` is the daemon's `ipc.socket` and `plugin-id` must be listed under `plugins` in the daemon's
configuration. Each defaults to the environment variable the daemon sets when it starts a plugin itself
(`IT_MATTERS_SOCKET`, `IT_MATTERS_PLUGIN_ID`), and then to /run/it-matters/plugins.sock and "demo".
Stop it with Ctrl-C: the devices then show as unreachable in the controller until it runs again.

With --connect it runs as a **remote agent** on another machine: it connects to the bridge's TCP listener
for this agent (`plugins[].connect.listen`), answers the bridge's challenge with a proof computed from the
token file (the token itself is never sent), and registers the same devices. See
docs/design/plugin-protocol.md for the handshake. (This demo does not reconnect by itself: run it in a loop
or under systemd, or use the Rust plugins, which do.)

The protocol is documented in docs/design/plugin-protocol.md.
"""
import hashlib
import hmac
import json
import os
import select
import socket
import sys
import time

MIC_FLIP_SECONDS = 20

sys.stdout.reconfigure(line_buffering=True)  # show output at once, even when piped


def parse_args(argv):
    """Returns (connect, plugin_id, token_file, socket_path)."""
    if "--connect" in argv:
        flags = dict(zip(argv[::2], argv[1::2]))
        plugin_id = flags.get("--plugin-id") or os.environ.get("IT_MATTERS_PLUGIN_ID")
        if not plugin_id:
            sys.exit("--plugin-id is required with --connect")
        return flags["--connect"], plugin_id, flags.get("--token-file"), None
    socket_path = (argv[0] if argv else os.environ.get("IT_MATTERS_SOCKET")) or "/run/it-matters/plugins.sock"
    plugin_id = (argv[1] if len(argv) > 1 else os.environ.get("IT_MATTERS_PLUGIN_ID")) or "demo"
    return None, plugin_id, None, socket_path


CONNECT, PLUGIN_ID, TOKEN_FILE, SOCKET = parse_args(sys.argv[1:])

if CONNECT:
    host, _, port = CONNECT.rpartition(":")
    sock = socket.create_connection((host, int(port)), timeout=5)
    sock.settimeout(None)
else:
    sock = socket.socket(socket.AF_UNIX)
    sock.connect(SOCKET)
lines = sock.makefile("rw")


def send(message):
    lines.write(json.dumps(message) + "\n")
    lines.flush()


def update(device, capability, value):
    send({"type": "update", "device": device, "capability": capability, "value": value})


# 1. Handshake: say who we are, expect "welcome" (an "error" means the plugin id is not configured).
hello = {"type": "hello", "plugin": PLUGIN_ID, "version": "demo", "proto": 1}
if CONNECT:
    # A remote agent is challenged first. With a token it answers with an HMAC proof (the token is not sent).
    challenge = json.loads(lines.readline() or "{}")
    if challenge.get("type") != "challenge":
        sys.exit(f"expected a challenge from the bridge, got: {challenge}")
    if TOKEN_FILE:
        token = open(TOKEN_FILE).readline().strip()
        message = b"it-matters-agent-v1\0" + challenge["nonce"].encode() + b"\0" + PLUGIN_ID.encode()
        hello["proof"] = hmac.new(token.encode(), message, hashlib.sha256).hexdigest()
send(hello)
reply = json.loads(lines.readline() or "{}")
if reply.get("type") != "welcome":
    sys.exit(f"the host refused the connection: {reply.get('message', reply)}")
print(f"connected to {CONNECT or SOCKET} as plugin {PLUGIN_ID!r}")

# 2. Register the devices. The host maps them onto Matter device types (see the protocol doc).
send({"type": "register", "device": {"id": "desktop-audio", "name": "Desktop Audio", "capabilities": [
    {"id": "volume", "type": "level", "min": 0, "max": 100, "writable": True},
    {"id": "mute", "type": "boolean", "writable": True, "role": "mute"}]}})
send({"type": "register", "device": {"id": "pc", "name": "Office PC", "capabilities": [
    {"id": "online", "type": "boolean"},
    {"id": "mic-active", "type": "boolean", "role": "occupancy"}]}})

# 3. Report the current values right away (until then the host reports defaults).
state = {"volume": 30, "mute": False, "mic-active": False}
update("desktop-audio", "volume", state["volume"])
update("desktop-audio", "mute", state["mute"])
update("pc", "online", True)
update("pc", "mic-active", state["mic-active"])

# 4. Serve: answer controller writes ("set") and heartbeats ("ping"), and flip the mic sensor.
last_flip = time.time()
while True:
    if time.time() - last_flip >= MIC_FLIP_SECONDS:
        last_flip = time.time()
        state["mic-active"] = not state["mic-active"]
        print("microphone active:", state["mic-active"])
        update("pc", "mic-active", state["mic-active"])
    ready, _, _ = select.select([sock], [], [], 0.5)
    if not ready:
        continue
    line = lines.readline()
    if not line:
        print("the host closed the connection")
        break
    message = json.loads(line)
    if message["type"] == "ping":
        send({"type": "pong"})
    elif message["type"] == "set":
        # A controller changed something: apply it, then confirm (or refuse with "ok": false).
        print("controller set", message["capability"], "=", message["value"])
        state[message["capability"]] = message["value"]
        send({"type": "set_result", "req_id": message["req_id"], "ok": True})
    else:
        print("host says:", message)

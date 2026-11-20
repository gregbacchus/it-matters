# Writing a plugin in Rust: `it-matters-plugin-sdk`

The SDK is a small async library (tokio) over the [plugin protocol](../design/plugin-protocol.md). It does
the handshake, answers the host's heartbeat pings itself, caps incoming lines, and turns controller writes
into requests you must answer. Any other language can speak the protocol directly (see
[`examples/plugins/demo.py`](../../examples/plugins/demo.py)).

**Status:** works; used by both bundled plugins. It is not published to crates.io; depend on it by path or
git.

## A complete plugin

```rust
use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec, Value};
use it_matters_plugin_sdk::{Env, Event, connect};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The daemon sets these when it starts the plugin (`run` in its configuration).
    let env = Env::from_env()?;
    let (client, mut events) = connect(&env.socket, &env.plugin_id, env!("CARGO_PKG_VERSION")).await?;

    client
        .register(DeviceSpec {
            id: "desk-lamp".into(),
            name: "Desk Lamp".into(),
            capabilities: vec![CapabilitySpec {
                id: "power".into(),
                kind: CapabilityKind::Boolean,
                writable: true,
                role: None,
            }],
        })
        .await?;
    client.update("desk-lamp", "power", Value::Bool(true)).await?; // send current values right away

    while let Some(event) = events.next().await {
        match event {
            Event::Set(request) => {
                // A controller changed something: apply it, then answer (accept or refuse).
                println!("{} = {:?}", request.capability, request.value);
                request.accept().await;
            }
            Event::HostError(message) => eprintln!("the host rejected something: {message}"),
            Event::Closed(reason) => return Err(reason.into()), // exit: the daemon restarts the plugin
        }
    }
    Ok(())
}
```

## API

| Item | Description |
|---|---|
| `Env::from_env()` | Reads `IT_MATTERS_SOCKET` and `IT_MATTERS_PLUGIN_ID`; an error says the daemon sets them. |
| `connect(socket, plugin_id, version)` | Connects and performs the handshake within 5 seconds. Errors: `Io` (cannot connect), `Refused` (the host said no, for example the id is not configured), `Protocol`, `Closed`. Returns a `Client` and the `Events`. |
| `Client::register(device)` | Registers (or re-registers) a device. Unmappable devices come back as `Event::HostError`. |
| `Client::update(device, capability, value)` | Reports a value. Send only changes, and the current values right after `register`. |
| `Client::unregister(device)` | Removes a device. |
| `Events::next()` | The next `Event`; `None` after `Closed`. |
| `Event::Set(SetRequest)` | A controller wrote a writable capability. `request.accept()` or `request.refuse(reason)` **must** be called, within the plugin's `timeout_ms`; the type is `#[must_use]`. |
| `Event::HostError(message)` | The host rejected something; the connection stays open. |
| `Event::Closed(reason)` | The connection ended. Exit; the daemon's supervisor restarts the plugin, which registers from scratch. |
| `connect_tcp(addr, plugin_id, version, token)` | Connects to a bridge's TCP listener as a **remote agent**: reads the host's challenge, answers with a proof from `token` (never sent), and does the handshake. A wrong or missing token is `Error::Refused("authentication failed")`. Not encrypted. |
| `Target`, `RemoteOptions` | Where to connect: `Target::Local { socket, plugin_id }` (started by the daemon) or `Target::Remote { addr, plugin_id, token }`. `RemoteOptions` holds the `--connect`, `--token-file` and `--plugin-id` flags every bundled plugin accepts and turns them (and the environment) into a `Target`. |
| `Connector` | `connect()` once; `reconnects()` is true for a remote target; `wait()` sleeps 1, 2, 4, ... 30 s; `reset()` after a good connection. A remote agent loops over these forever (and re-registers each time); a local plugin just exits and lets the daemon restart it. |
| `sanitize_id(text, fallback)` | Turns text such as a host name into a valid id. |

Rules of thumb: one task should own the thing you control (so a poll and a write never overlap, as in
`it-matters-plugin-linux-audio`); send only changes; keep the last value when a read fails and retry.

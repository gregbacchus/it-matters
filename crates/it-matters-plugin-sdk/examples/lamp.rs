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

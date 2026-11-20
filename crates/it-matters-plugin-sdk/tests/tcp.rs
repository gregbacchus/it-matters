//! The SDK's remote-agent connection against the real host over real TCP (loopback).

use std::collections::BTreeMap;
use std::time::Duration;

use it_matters_core::allocator::{Allocator, MemoryStore};
use it_matters_core::frontend::{self, FakeFrontend, FrontendEvent};
use it_matters_core::host::{HostSettings, PluginSettings, serve_tcp, spawn_host};
use it_matters_core::registry::Registry;
use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec};
use it_matters_plugin_sdk::{Connector, Error, Event, Target, connect_tcp};
use tokio::net::TcpListener;
use tokio::time::timeout;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct Bridge {
    addr: String,
    fake: FakeFrontend,
}

async fn bridge(token: Option<&str>) -> Bridge {
    let mut settings = HostSettings::new(BTreeMap::from([(
        "agent".to_owned(),
        PluginSettings { set_timeout: Duration::from_secs(2) },
    )]));
    settings.remote_plugins.insert("agent".into());
    settings.auth_failure_delay = Duration::from_millis(20);
    let (host_end, frontend_end) = frontend::channel();
    let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
    let (host, _actor) = spawn_host(settings, registry, host_end);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(serve_tcp(listener, host, "agent".into(), token.map(|t| t.as_bytes().to_vec()), 4));
    Bridge { addr, fake: FakeFrontend::new(frontend_end) }
}

fn pc() -> DeviceSpec {
    DeviceSpec {
        id: "pc".into(),
        name: "PC".into(),
        capabilities: vec![CapabilitySpec {
            id: "online".into(),
            kind: CapabilityKind::Boolean,
            writable: false,
            role: None,
        }],
    }
}

async fn event(b: &Bridge) -> FrontendEvent {
    timeout(Duration::from_secs(3), b.fake.next_event()).await.expect("no event").unwrap()
}

#[tokio::test]
async fn an_agent_with_the_right_token_connects_and_registers() {
    let b = bridge(Some(TOKEN)).await;
    let (client, _events) = connect_tcp(&b.addr, "agent", "test", Some(TOKEN)).await.unwrap();
    client.register(pc()).await.unwrap();
    assert!(matches!(event(&b).await, FrontendEvent::AddEndpoint(_)));
}

#[tokio::test]
async fn a_wrong_or_missing_token_is_refused_with_a_helpful_message() {
    let b = bridge(Some(TOKEN)).await;
    for token in [Some("ffffffffffffffffffffffffffffffff"), None] {
        let err = connect_tcp(&b.addr, "agent", "test", token).await.err().expect("must be refused");
        assert!(matches!(&err, Error::Refused(m) if m == "authentication failed"), "{err}");
        assert!(err.to_string().contains("check the token file"), "{err}");
    }
}

#[tokio::test]
async fn the_wrong_plugin_id_is_refused_the_same_way() {
    let b = bridge(Some(TOKEN)).await;
    let err = connect_tcp(&b.addr, "other", "test", Some(TOKEN)).await.err().expect("must be refused");
    assert!(matches!(&err, Error::Refused(m) if m == "authentication failed"), "{err}");
}

#[tokio::test]
async fn a_listener_without_a_token_accepts_an_agent_with_or_without_one() {
    let b = bridge(None).await;
    assert!(connect_tcp(&b.addr, "agent", "test", None).await.is_ok());
    let b = bridge(None).await;
    assert!(connect_tcp(&b.addr, "agent", "test", Some(TOKEN)).await.is_ok());
}

#[tokio::test]
async fn an_unreachable_bridge_is_a_clear_io_error() {
    // Nothing listens on this port (it was just freed).
    let free = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
    let err = connect_tcp(&free, "agent", "test", None).await.err().unwrap();
    assert!(matches!(err, Error::Io(_)) && err.to_string().contains(&free), "{err}");
}

#[tokio::test]
async fn something_that_is_not_a_bridge_is_a_protocol_error() {
    use tokio::io::AsyncWriteExt;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let err = connect_tcp(&addr, "agent", "test", Some(TOKEN)).await.err().unwrap();
    assert!(matches!(err, Error::Protocol(_)), "{err}");
}

#[tokio::test]
async fn the_connector_reconnects_a_remote_agent_and_the_new_session_replaces_the_old() {
    let b = bridge(Some(TOKEN)).await;
    let target = Target::Remote { addr: b.addr.clone(), plugin_id: "agent".into(), token: Some(TOKEN.into()) };
    let mut connector = Connector::new(target, "test");
    assert!(connector.reconnects());

    let (client, mut first) = connector.connect().await.unwrap();
    client.register(pc()).await.unwrap();
    assert!(matches!(event(&b).await, FrontendEvent::AddEndpoint(_)));
    connector.reset();

    // The computer rebooted: it connects again while the bridge still holds the old (dead) session.
    connector.wait().await;
    let (client2, _second) = connector.connect().await.expect("the new session replaces the stale one");
    assert!(matches!(first.next().await, Some(Event::Closed(_))), "the old session is closed by the bridge");
    client2.register(pc()).await.unwrap();
    assert_eq!(event(&b).await, FrontendEvent::SetReachable { endpoint: 2, reachable: true }, "the same endpoint");
}

#[tokio::test]
async fn a_local_target_does_not_reconnect() {
    let t = Target::Local { socket: "/x".into(), plugin_id: "p".into() };
    assert!(!Connector::new(t, "t").reconnects());
}

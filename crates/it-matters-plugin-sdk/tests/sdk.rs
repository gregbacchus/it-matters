//! The SDK against the real host over a real Unix socket, with a fake Matter side.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use it_matters_core::allocator::{Allocator, MemoryStore};
use it_matters_core::frontend::{self, AttrValue, FakeFrontend, FrontendEvent, Write};
use it_matters_core::host::{HostSettings, PluginSettings, bind_unix, serve_unix, spawn_host};
use it_matters_core::registry::Registry;
use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec, Role, Value};
use it_matters_plugin_sdk::{Client, Error, Event, Events, connect};
use tokio::time::{sleep, timeout};

struct World {
    socket: PathBuf,
    fake: FakeFrontend,
    dir: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn world(name: &str, tweak: impl FnOnce(&mut HostSettings)) -> World {
    let dir = std::env::temp_dir().join(format!("it-matters-sdk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("p.sock");
    let (host_end, frontend_end) = frontend::channel();
    let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
    let mut settings = HostSettings::new(BTreeMap::from([(
        "demo".to_owned(),
        PluginSettings { set_timeout: Duration::from_secs(2) },
    )]));
    tweak(&mut settings);
    let (host, _actor) = spawn_host(settings, registry, host_end);
    let listener = bind_unix(&socket).unwrap();
    tokio::spawn(serve_unix(listener, host));
    World { socket, fake: FakeFrontend::new(frontend_end), dir }
}

fn cap(id: &str, kind: CapabilityKind, writable: bool, role: Option<Role>) -> CapabilitySpec {
    CapabilitySpec { id: id.into(), kind, writable, role }
}

fn speaker() -> DeviceSpec {
    DeviceSpec {
        id: "audio".into(),
        name: "Audio".into(),
        capabilities: vec![
            cap("volume", CapabilityKind::Level { min: 0, max: 100 }, true, None),
            cap("mute", CapabilityKind::Boolean, true, Some(Role::Mute)),
        ],
    }
}

async fn next_event(w: &World) -> FrontendEvent {
    timeout(Duration::from_secs(3), w.fake.next_event()).await.expect("no frontend event").unwrap()
}

async fn next_plugin_event(events: &mut Events) -> Event {
    timeout(Duration::from_secs(3), events.next()).await.expect("no plugin event").expect("events ended")
}

async fn plugin(w: &World) -> (Client, Events) {
    connect(&w.socket, "demo", "test").await.unwrap()
}

#[tokio::test]
async fn register_and_update_reach_the_matter_side() {
    let w = world("reg", |_| {}).await;
    let (client, _events) = plugin(&w).await;
    client.register(speaker()).await.unwrap();
    assert!(matches!(next_event(&w).await, FrontendEvent::AddEndpoint(_)));
    client.update("audio", "volume", Value::Int(100)).await.unwrap();
    assert_eq!(next_event(&w).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::CurrentLevel(254) });
}

#[tokio::test]
async fn a_controller_write_becomes_a_set_request_the_plugin_can_accept() {
    let w = world("accept", |_| {}).await;
    let (client, mut events) = plugin(&w).await;
    client.register(speaker()).await.unwrap();
    next_event(&w).await;
    let answer = w.fake.write(2, Write::OnOff(false)).await; // Off = muted
    let Event::Set(request) = next_plugin_event(&mut events).await else { panic!("expected a set") };
    assert_eq!(
        (request.device.as_str(), request.capability.as_str(), request.value),
        ("audio", "mute", Value::Bool(true))
    );
    request.accept().await;
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
}

#[tokio::test]
async fn a_refused_set_request_tells_the_controller_why() {
    let w = world("refuse", |_| {}).await;
    let (client, mut events) = plugin(&w).await;
    client.register(speaker()).await.unwrap();
    next_event(&w).await;
    let answer = w.fake.write(2, Write::CurrentLevel(10)).await;
    let Event::Set(request) = next_plugin_event(&mut events).await else { panic!("expected a set") };
    request.refuse("no such sink").await;
    assert_eq!(answer.recv().await.unwrap(), Err("no such sink".to_owned()));
}

#[tokio::test]
async fn an_unlisted_plugin_is_refused_with_the_hosts_reason() {
    let w = world("refused", |_| {}).await;
    let err = connect(&w.socket, "intruder", "test").await.err().expect("must be refused");
    assert!(matches!(&err, Error::Refused(m) if m.contains("not configured")), "{err}");
}

#[tokio::test]
async fn a_missing_socket_is_a_clear_error() {
    let err = connect(std::path::Path::new("/nonexistent/p.sock"), "demo", "t").await.err().unwrap();
    assert!(err.to_string().contains("/nonexistent/p.sock"), "{err}");
}

#[tokio::test]
async fn host_errors_are_surfaced_and_the_connection_stays_usable() {
    let w = world("hosterr", |_| {}).await;
    let (client, mut events) = plugin(&w).await;
    let mut bad = speaker();
    bad.capabilities.pop(); // a level without a mute cannot be mapped
    client.register(bad).await.unwrap();
    assert!(matches!(next_plugin_event(&mut events).await, Event::HostError(m) if m.contains("needs a boolean")));
    client.register(speaker()).await.unwrap();
    assert!(matches!(next_event(&w).await, FrontendEvent::AddEndpoint(_)));
}

#[tokio::test]
async fn the_sdk_answers_heartbeats_so_an_idle_plugin_is_not_dropped() {
    let w = world("ping", |s| {
        s.ping_interval = Duration::from_millis(50);
        s.idle_timeout = Duration::from_millis(300);
    })
    .await;
    let (client, mut events) = plugin(&w).await;
    // Several idle periods pass; the SDK's automatic pongs keep the session alive.
    let idle = async {
        loop {
            if let Some(Event::Closed(r)) = events.next().await {
                return r;
            }
        }
    };
    assert!(
        timeout(Duration::from_millis(1200), idle).await.is_err(),
        "the host dropped an idle but responsive plugin"
    );
    client.register(speaker()).await.unwrap();
    assert!(matches!(next_event(&w).await, FrontendEvent::AddEndpoint(_)));
}

#[tokio::test]
async fn closing_is_reported_so_the_plugin_can_exit() {
    let w = world("closed", |s| {
        s.ping_interval = Duration::from_secs(60);
        s.idle_timeout = Duration::from_millis(150);
    })
    .await;
    // Not answering pings is impossible with the SDK, but a host that goes away is easy: here the idle
    // timeout does it, because the SDK cannot send anything it was not asked to.
    let (_client, mut events) = plugin(&w).await;
    let Event::Closed(reason) = next_plugin_event(&mut events).await else {
        panic!("expected the connection to close")
    };
    assert!(!reason.is_empty());
    assert!(events.next().await.is_none(), "nothing follows Closed");
    sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn sending_after_the_connection_closed_fails_instead_of_hanging() {
    let w = world("sendclosed", |s| {
        s.ping_interval = Duration::from_secs(60);
        s.idle_timeout = Duration::from_millis(100);
    })
    .await;
    let (client, mut events) = plugin(&w).await;
    assert!(matches!(next_plugin_event(&mut events).await, Event::Closed(_)));
    sleep(Duration::from_millis(50)).await;
    let mut failed = false;
    for _ in 0..50 {
        if client.update("audio", "volume", Value::Int(1)).await.is_err() {
            failed = true;
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert!(failed, "updates must fail once the connection is gone");
}

//! The plugin loop against the real host (over a Unix socket) with a scripted session source.
//! `real_logind_*` need a system bus with systemd-logind: run them with `cargo test -- --ignored`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use it_matters_core::allocator::{Allocator, MemoryStore};
use it_matters_core::frontend::{self, AttrValue, FakeFrontend, FrontendEvent};
use it_matters_core::host::{HostSettings, PluginSettings, bind_unix, serve_unix, spawn_host};
use it_matters_core::registry::Registry;
use it_matters_plugin_linux_system::{SessionInfo, SessionSource, run};
use it_matters_plugin_sdk::connect;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

#[derive(Clone, Default)]
struct Fake {
    sessions: Arc<Mutex<Vec<SessionInfo>>>,
    changed: Arc<Notify>,
    failing: Arc<AtomicBool>,
}

impl SessionSource for Fake {
    async fn sessions(&mut self) -> Result<Vec<SessionInfo>, String> {
        if self.failing.load(Ordering::SeqCst) {
            return Err("logind is restarting".into());
        }
        Ok(self.sessions.lock().unwrap().clone())
    }
    async fn changed(&mut self) {
        self.changed.notified().await;
    }
}

fn desktop() -> SessionInfo {
    SessionInfo { class: "user".into(), kind: "wayland".into(), state: "active".into(), remote: false }
}

struct Setup {
    fake: Fake,
    frontend: FakeFrontend,
    dir: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start(name: &str, poll: Duration) -> Setup {
    let dir = std::env::temp_dir().join(format!("it-matters-sys-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("p.sock");
    let (host_end, frontend_end) = frontend::channel();
    let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
    let settings = HostSettings::new(BTreeMap::from([(
        "linux-system".to_owned(),
        PluginSettings { set_timeout: Duration::from_secs(2) },
    )]));
    let (host, _actor) = spawn_host(settings, registry, host_end);
    tokio::spawn(serve_unix(bind_unix(&socket).unwrap(), host));

    let fake = Fake::default();
    let mut source = fake.clone();
    tokio::spawn(async move {
        let (client, mut events) = connect(&socket, "linux-system", "test").await.unwrap();
        run(&client, &mut events, &mut source, "office-pc", "Office PC", poll).await;
    });
    Setup { fake, frontend: FakeFrontend::new(frontend_end), dir }
}

async fn event(s: &Setup) -> FrontendEvent {
    timeout(Duration::from_secs(3), s.frontend.next_event()).await.expect("no event").unwrap()
}

/// Skips the start-up events: two endpoints, `online` = true and the initial `signed-in` = false.
async fn settle(s: &Setup) {
    for _ in 0..2 {
        assert!(matches!(event(s).await, FrontendEvent::AddEndpoint(_)));
    }
    assert_eq!(event(s).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::BooleanState(true) });
    assert_eq!(event(s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::BooleanState(false) });
}

#[tokio::test]
async fn it_registers_two_sensors_and_reports_online() {
    let s = start("reg", Duration::from_secs(60)).await;
    let FrontendEvent::AddEndpoint(first) = event(&s).await else { panic!() };
    let FrontendEvent::AddEndpoint(second) = event(&s).await else { panic!() };
    assert_eq!(
        (first.endpoint, first.name.as_str(), first.unique_id.as_str()),
        (2, "Office PC online", "office-pc/online")
    );
    assert_eq!((second.endpoint, second.unique_id.as_str()), (3, "office-pc/signed-in"));
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::BooleanState(true) });
}

#[tokio::test]
async fn signing_in_and_out_is_reported_when_logind_signals_a_change() {
    let s = start("signal", Duration::from_secs(60)).await; // the poll is far away: only the signal can trigger
    settle(&s).await;
    s.fake.sessions.lock().unwrap().push(desktop());
    s.fake.changed.notify_one();
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::BooleanState(true) });
    s.fake.sessions.lock().unwrap().clear();
    s.fake.changed.notify_one();
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::BooleanState(false) });
}

#[tokio::test]
async fn state_changes_without_a_signal_are_caught_by_the_poll() {
    let s = start("poll", Duration::from_millis(50)).await;
    settle(&s).await;
    s.fake.sessions.lock().unwrap().push(desktop());
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::BooleanState(true) });
}

#[tokio::test]
async fn unchanged_values_are_not_resent_and_a_failed_read_keeps_the_last_value() {
    let s = start("quiet", Duration::from_millis(20)).await;
    settle(&s).await;
    s.fake.sessions.lock().unwrap().push(desktop());
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::BooleanState(true) });

    // Many polls with the same answer: nothing more is sent (the registry would drop duplicates
    // too, but the plugin should not even send them).
    sleep(Duration::from_millis(200)).await;
    assert!(s.frontend.drain().is_empty());

    // logind goes away: the last value stays and nothing is reported.
    s.fake.failing.store(true, Ordering::SeqCst);
    s.fake.sessions.lock().unwrap().clear();
    sleep(Duration::from_millis(200)).await;
    assert!(s.frontend.drain().is_empty(), "a failed read must not flip the sensor");

    // It comes back and the real (changed) state is reported.
    s.fake.failing.store(false, Ordering::SeqCst);
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::BooleanState(false) });
}

#[tokio::test]
#[ignore = "needs the system D-Bus with systemd-logind"]
async fn real_logind_lists_this_machines_sessions() {
    use it_matters_plugin_linux_system::logind::Logind;
    let mut logind = Logind::connect().await.expect("logind");
    let sessions = logind.sessions().await.expect("sessions");
    println!("{sessions:#?}");
    assert!(!sessions.is_empty(), "a logged-in developer has at least one session");
}

/// A bare-bones host that completes the handshake and records every line the plugin sends, so the test
/// can see what is on the wire (the real host would silently drop duplicate values).
async fn recording_host(socket: PathBuf) -> Arc<Mutex<Vec<String>>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = bind_unix(&socket).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::new(rd).lines();
        assert!(rd.next_line().await.unwrap().unwrap().contains("\"type\":\"hello\""));
        wr.write_all(b"{\"type\":\"welcome\",\"proto\":1}\n").await.unwrap();
        while let Ok(Some(line)) = rd.next_line().await {
            sink.lock().unwrap().push(line);
        }
    });
    lines
}

#[tokio::test]
async fn only_changes_are_sent_over_the_wire() {
    let dir = std::env::temp_dir().join(format!("it-matters-sys-wire-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("p.sock");
    let lines = recording_host(socket.clone()).await;

    let fake = Fake::default();
    let mut source = fake.clone();
    tokio::spawn(async move {
        let (client, mut events) = connect(&socket, "linux-system", "test").await.unwrap();
        run(&client, &mut events, &mut source, "office-pc", "Office PC", Duration::from_millis(10)).await;
    });
    let signed_in_updates = || {
        lines.lock().unwrap().iter().filter(|l| l.contains("\"capability\":\"signed-in\"")).cloned().collect::<Vec<_>>()
    };
    sleep(Duration::from_millis(300)).await; // dozens of polls, same answer
    assert_eq!(signed_in_updates().len(), 1, "one initial value, then silence: {:?}", signed_in_updates());
    for _ in 0..5 {
        fake.changed.notify_one(); // signals without a change
        sleep(Duration::from_millis(15)).await;
    }
    assert_eq!(signed_in_updates().len(), 1);
    fake.sessions.lock().unwrap().push(desktop());
    sleep(Duration::from_millis(200)).await;
    assert_eq!(signed_in_updates().len(), 2, "a real change is sent once");
    assert!(signed_in_updates()[1].contains("\"value\":true"));
    let _ = std::fs::remove_dir_all(&dir);
}

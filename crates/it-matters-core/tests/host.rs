//! Host integration tests: real wire protocol over in-memory pipes (with virtual time), plus one
//! test over a real Unix socket. The Matter side is the `FakeFrontend`.

use std::collections::BTreeMap;
use std::time::Duration;

use it_matters_core::allocator::{Allocator, MemoryStore};
use it_matters_core::frontend::{self, AttrValue, FakeFrontend, FrontendEvent, Write};
use it_matters_core::host::{HostHandle, HostSettings, PluginSettings, spawn_host};
use it_matters_core::mapping::UnitKind;
use it_matters_core::registry::Registry;
use it_matters_model::protocol::{HostMessage, MAX_FRAME_BYTES, PluginMessage, decode_line, encode_line};
use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec, Role, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const SET_TIMEOUT: Duration = Duration::from_secs(2);

struct Fixture {
    host: HostHandle,
    fake: FakeFrontend,
    _actor: JoinHandle<()>,
}

fn settings() -> HostSettings {
    let one = || PluginSettings { set_timeout: SET_TIMEOUT };
    HostSettings::new(BTreeMap::from([("p".to_owned(), one()), ("q".to_owned(), one())]))
}

fn fixture_with(settings: HostSettings) -> Fixture {
    let (host_end, frontend_end) = frontend::channel();
    let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
    let (host, actor) = spawn_host(settings, registry, host_end);
    Fixture { host, fake: FakeFrontend::new(frontend_end), _actor: actor }
}

fn fixture() -> Fixture {
    fixture_with(settings())
}

struct Client {
    rd: BufReader<ReadHalf<DuplexStream>>,
    wr: WriteHalf<DuplexStream>,
}

impl Client {
    fn connect(f: &Fixture) -> Client {
        let (a, b) = tokio::io::duplex(1 << 20);
        f.host.accept(b);
        let (rd, wr) = tokio::io::split(a);
        Client { rd: BufReader::new(rd), wr }
    }

    async fn send(&mut self, m: &PluginMessage) {
        self.raw(&encode_line(m).unwrap()).await;
    }

    async fn raw(&mut self, text: &str) {
        self.wr.write_all(text.as_bytes()).await.unwrap();
    }

    /// The next host message, or `None` once the host closed the connection.
    async fn recv(&mut self) -> Option<HostMessage> {
        let mut line = String::new();
        match self.rd.read_line(&mut line).await {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(decode_line(&line).unwrap()),
        }
    }

    /// The next message that is not a heartbeat ping.
    async fn recv_skip_ping(&mut self) -> Option<HostMessage> {
        loop {
            match self.recv().await? {
                HostMessage::Ping => continue,
                other => return Some(other),
            }
        }
    }

    async fn hello_as(f: &Fixture, plugin: &str) -> Client {
        let mut c = Client::connect(f);
        c.send(&hello(plugin)).await;
        assert_eq!(c.recv().await, Some(HostMessage::Welcome { proto: 1 }));
        c
    }
}

fn hello(plugin: &str) -> PluginMessage {
    PluginMessage::Hello { plugin: plugin.into(), version: "test".into(), proto: 1, proof: None }
}

fn cap(id: &str, kind: CapabilityKind, writable: bool, role: Option<Role>) -> CapabilitySpec {
    CapabilitySpec { id: id.into(), kind, writable, role }
}

fn audio() -> DeviceSpec {
    DeviceSpec {
        id: "desktop-audio".into(),
        name: "Desktop Audio".into(),
        capabilities: vec![
            cap("volume", CapabilityKind::Level { min: 0, max: 100 }, true, None),
            cap("mute", CapabilityKind::Boolean, true, Some(Role::Mute)),
        ],
    }
}

fn pc() -> DeviceSpec {
    DeviceSpec {
        id: "pc".into(),
        name: "PC".into(),
        capabilities: vec![cap("online", CapabilityKind::Boolean, false, None)],
    }
}

async fn event(f: &Fixture) -> FrontendEvent {
    timeout(Duration::from_secs(1), f.fake.next_event()).await.expect("no event arrived").expect("frontend closed")
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test(start_paused = true)]
async fn register_and_update_reach_the_frontend() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    c.send(&PluginMessage::Register { device: audio() }).await;
    let FrontendEvent::AddEndpoint(ep) = event(&f).await else { panic!("expected AddEndpoint") };
    assert_eq!((ep.endpoint, ep.kind, ep.reachable), (2, UnitKind::Speaker, true));
    c.send(&PluginMessage::Update {
        device: "desktop-audio".into(),
        capability: "volume".into(),
        value: Value::Int(100),
    })
    .await;
    assert_eq!(event(&f).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::CurrentLevel(254) });
}

#[tokio::test(start_paused = true)]
async fn unlisted_plugins_are_refused() {
    let f = fixture();
    let mut c = Client::connect(&f);
    c.send(&hello("intruder")).await;
    assert!(matches!(c.recv().await, Some(HostMessage::Error { message }) if message.contains("not configured")));
    assert_eq!(c.recv().await, None, "the connection is closed");
}

#[tokio::test(start_paused = true)]
async fn handshake_problems_close_the_connection() {
    let f = fixture();
    // Not a hello.
    let mut c = Client::connect(&f);
    c.send(&PluginMessage::Ping).await;
    assert!(matches!(c.recv().await, Some(HostMessage::Error { message }) if message.contains("must be hello")));
    assert_eq!(c.recv().await, None);
    // Wrong protocol version.
    let mut c = Client::connect(&f);
    c.send(&PluginMessage::Hello { plugin: "p".into(), version: "x".into(), proto: 99, proof: None }).await;
    assert!(matches!(c.recv().await, Some(HostMessage::Error { message }) if message.contains("unsupported protocol")));
    assert_eq!(c.recv().await, None);
    // Garbage.
    let mut c = Client::connect(&f);
    c.raw("this is not json\n").await;
    assert!(matches!(c.recv().await, Some(HostMessage::Error { .. })));
    assert_eq!(c.recv().await, None);
}

#[tokio::test(start_paused = true)]
async fn a_silent_connection_times_out_during_the_handshake() {
    let f = fixture();
    let mut c = Client::connect(&f);
    // No hello is ever sent; virtual time runs forward until the host gives up.
    assert!(matches!(c.recv().await, Some(HostMessage::Error { message }) if message.contains("timed out")));
    assert_eq!(c.recv().await, None);
}

#[tokio::test(start_paused = true)]
async fn oversized_frames_close_the_session() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    c.raw(&"x".repeat(MAX_FRAME_BYTES + 10)).await;
    c.raw("\n").await;
    assert!(matches!(c.recv_skip_ping().await, Some(HostMessage::Error { message }) if message.contains("too long")));
    assert_eq!(c.recv_skip_ping().await, None);
}

#[tokio::test(start_paused = true)]
async fn a_line_that_never_ends_is_cut_off_instead_of_buffered() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    // No newline at all: the host must not wait for one while memory grows.
    c.raw(&"x".repeat(MAX_FRAME_BYTES + 10)).await;
    assert!(matches!(c.recv_skip_ping().await, Some(HostMessage::Error { message }) if message.contains("too long")));
    assert_eq!(c.recv_skip_ping().await, None);
}

#[tokio::test(start_paused = true)]
async fn a_malformed_message_after_the_handshake_closes_the_session() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    c.raw("{\"type\":\"explode\"}\n").await;
    assert!(matches!(c.recv_skip_ping().await, Some(HostMessage::Error { .. })));
    assert_eq!(c.recv_skip_ping().await, None);
}

#[tokio::test(start_paused = true)]
async fn a_second_live_session_for_a_plugin_is_refused_and_the_first_is_unaffected() {
    let f = fixture();
    let mut first = Client::hello_as(&f, "p").await;
    let mut second = Client::connect(&f);
    second.send(&hello("p")).await;
    assert!(
        matches!(second.recv().await, Some(HostMessage::Error { message }) if message.contains("already connected"))
    );
    assert_eq!(second.recv().await, None);
    first.send(&PluginMessage::Register { device: pc() }).await;
    assert!(matches!(event(&f).await, FrontendEvent::AddEndpoint(_)));
    assert!(f.fake.drain().is_empty());
}

#[tokio::test(start_paused = true)]
async fn disconnect_marks_devices_unreachable_and_reconnect_restores_them() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    c.send(&PluginMessage::Register { device: pc() }).await;
    assert!(matches!(event(&f).await, FrontendEvent::AddEndpoint(_)));
    drop(c);
    assert_eq!(event(&f).await, FrontendEvent::SetReachable { endpoint: 2, reachable: false });

    let mut c = Client::hello_as(&f, "p").await;
    c.send(&PluginMessage::Register { device: pc() }).await;
    assert_eq!(event(&f).await, FrontendEvent::SetReachable { endpoint: 2, reachable: true }, "same endpoint reused");
    settle().await;
    assert!(f.fake.drain().is_empty(), "no new endpoint was created");
}

#[tokio::test(start_paused = true)]
async fn a_device_cannot_be_claimed_by_a_second_plugin() {
    let f = fixture();
    let mut p = Client::hello_as(&f, "p").await;
    let mut q = Client::hello_as(&f, "q").await;
    p.send(&PluginMessage::Register { device: pc() }).await;
    assert!(matches!(event(&f).await, FrontendEvent::AddEndpoint(_)));
    q.send(&PluginMessage::Register { device: pc() }).await;
    assert!(matches!(q.recv().await, Some(HostMessage::Error { message }) if message.contains("already registered")));
    // The connection stays usable.
    q.send(&PluginMessage::Ping).await;
    assert_eq!(q.recv().await, Some(HostMessage::Pong));
    assert!(f.fake.drain().is_empty());
}

#[tokio::test(start_paused = true)]
async fn invalid_registrations_and_updates_are_reported_but_not_fatal() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    let mut bad = audio();
    bad.capabilities.pop();
    c.send(&PluginMessage::Register { device: bad }).await;
    assert!(matches!(c.recv().await, Some(HostMessage::Error { message }) if message.contains("needs a boolean")));
    c.send(&PluginMessage::Update { device: "ghost".into(), capability: "x".into(), value: Value::Bool(true) }).await;
    assert!(matches!(c.recv().await, Some(HostMessage::Error { message }) if message.contains("not registered")));
    c.send(&PluginMessage::Ping).await;
    assert_eq!(c.recv().await, Some(HostMessage::Pong));
}

#[tokio::test(start_paused = true)]
async fn unregister_removes_the_endpoints() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    c.send(&PluginMessage::Register { device: pc() }).await;
    event(&f).await;
    c.send(&PluginMessage::Unregister { device: "pc".into() }).await;
    assert_eq!(event(&f).await, FrontendEvent::RemoveEndpoint { endpoint: 2 });
}

// ---- controller writes ------------------------------------------------------------------------

async fn registered_audio(f: &Fixture) -> Client {
    let mut c = Client::hello_as(f, "p").await;
    c.send(&PluginMessage::Register { device: audio() }).await;
    event(f).await;
    c
}

#[tokio::test(start_paused = true)]
async fn a_controller_write_round_trips_through_the_plugin() {
    let f = fixture();
    let mut c = registered_audio(&f).await;
    let answer = f.fake.write(2, Write::CurrentLevel(254)).await;
    let Some(HostMessage::Set { req_id, device, capability, value }) = c.recv_skip_ping().await else {
        panic!("expected a set")
    };
    assert_eq!((device.as_str(), capability.as_str(), value), ("desktop-audio", "volume", Value::Int(100)));
    c.send(&PluginMessage::SetResult { req_id, ok: true, error: None }).await;
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
    assert_eq!(event(&f).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::CurrentLevel(254) });
}

#[tokio::test(start_paused = true)]
async fn muting_from_a_controller_sends_mute_true() {
    let f = fixture();
    let mut c = registered_audio(&f).await;
    let answer = f.fake.write(2, Write::OnOff(false)).await; // Off = muted
    let Some(HostMessage::Set { req_id, capability, value, .. }) = c.recv_skip_ping().await else { panic!() };
    assert_eq!((capability.as_str(), value), ("mute", Value::Bool(true)));
    c.send(&PluginMessage::SetResult { req_id, ok: true, error: None }).await;
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn a_refused_write_reports_the_reason_and_changes_nothing() {
    let f = fixture();
    let mut c = registered_audio(&f).await;
    let answer = f.fake.write(2, Write::OnOff(false)).await;
    let Some(HostMessage::Set { req_id, .. }) = c.recv_skip_ping().await else { panic!() };
    c.send(&PluginMessage::SetResult { req_id, ok: false, error: Some("no such sink".into()) }).await;
    assert_eq!(answer.recv().await.unwrap(), Err("no such sink".to_owned()));
    settle().await;
    assert!(f.fake.drain().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_plugin_that_does_not_answer_a_write_times_out() {
    let f = fixture();
    let mut c = registered_audio(&f).await;
    let answer = f.fake.write(2, Write::OnOff(false)).await;
    assert!(matches!(c.recv_skip_ping().await, Some(HostMessage::Set { .. })));
    // The plugin stays silent; virtual time moves on past `set_timeout`.
    assert_eq!(answer.recv().await.unwrap(), Err("the plugin did not answer in time".to_owned()));
    // A late answer is ignored.
    c.send(&PluginMessage::SetResult { req_id: 1, ok: true, error: None }).await;
    settle().await;
    assert!(f.fake.drain().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_disconnect_fails_the_writes_in_flight() {
    let f = fixture();
    let mut c = registered_audio(&f).await;
    let answer = f.fake.write(2, Write::OnOff(false)).await;
    assert!(matches!(c.recv_skip_ping().await, Some(HostMessage::Set { .. })));
    drop(c);
    assert_eq!(answer.recv().await.unwrap(), Err("plugin disconnected".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn writes_to_unknown_or_unreachable_endpoints_are_refused_without_bothering_a_plugin() {
    let f = fixture();
    let c = registered_audio(&f).await;
    let answer = f.fake.write(99, Write::OnOff(true)).await;
    assert!(answer.recv().await.unwrap().unwrap_err().contains("no device"));
    drop(c);
    assert!(matches!(event(&f).await, FrontendEvent::SetReachable { reachable: false, .. }));
    let answer = f.fake.write(2, Write::OnOff(true)).await;
    assert!(answer.recv().await.unwrap().unwrap_err().contains("not reachable"));
}

#[tokio::test(start_paused = true)]
async fn only_the_addressed_plugin_can_answer_a_write() {
    let f = fixture();
    let mut p = registered_audio(&f).await;
    let mut q = Client::hello_as(&f, "q").await;
    let answer = f.fake.write(2, Write::OnOff(false)).await;
    let Some(HostMessage::Set { req_id, .. }) = p.recv_skip_ping().await else { panic!() };
    q.send(&PluginMessage::SetResult { req_id, ok: true, error: None }).await;
    settle().await;
    assert!(answer.try_recv().is_err(), "q's answer must not complete p's request");
    p.send(&PluginMessage::SetResult { req_id, ok: false, error: Some("nope".into()) }).await;
    assert_eq!(answer.recv().await.unwrap(), Err("nope".to_owned()));
}

// ---- heartbeat --------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_host_pings_and_a_responsive_plugin_stays_connected() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    for _ in 0..10 {
        // 10 pings = 100 s of virtual time, far past the 30 s idle timeout.
        assert_eq!(c.recv().await, Some(HostMessage::Ping));
        c.send(&PluginMessage::Pong).await;
    }
    c.send(&PluginMessage::Register { device: pc() }).await;
    assert!(matches!(event(&f).await, FrontendEvent::AddEndpoint(_)));
}

#[tokio::test(start_paused = true)]
async fn a_plugin_that_goes_silent_is_dropped_and_its_devices_become_unreachable() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    c.send(&PluginMessage::Register { device: pc() }).await;
    event(&f).await;
    // Read pings but never answer: after the idle timeout the host closes the connection.
    let drain = async {
        while let Some(m) = c.recv().await {
            assert_eq!(m, HostMessage::Ping);
        }
    };
    timeout(Duration::from_secs(120), drain).await.expect("the host never dropped the silent plugin");
    assert_eq!(event(&f).await, FrontendEvent::SetReachable { endpoint: 2, reachable: false });
}

#[tokio::test(start_paused = true)]
async fn plugin_pings_are_answered() {
    let f = fixture();
    let mut c = Client::hello_as(&f, "p").await;
    c.send(&PluginMessage::Ping).await;
    assert_eq!(c.recv().await, Some(HostMessage::Pong));
}

// ---- a real Unix socket -----------------------------------------------------------------------

#[tokio::test]
async fn the_unix_socket_listener_serves_plugins() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixStream;

    let dir = std::env::temp_dir().join(format!("it-matters-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("run/plugins.sock");
    let f = fixture();

    let listener = it_matters_core::host::bind_unix(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o660);
    let server = tokio::spawn(it_matters_core::host::serve_unix(listener, f.host.clone()));

    let stream = UnixStream::connect(&path).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    wr.write_all(encode_line(&hello("p")).unwrap().as_bytes()).await.unwrap();
    let mut line = String::new();
    rd.read_line(&mut line).await.unwrap();
    assert_eq!(decode_line::<HostMessage>(&line).unwrap(), HostMessage::Welcome { proto: 1 });
    wr.write_all(encode_line(&PluginMessage::Register { device: pc() }).unwrap().as_bytes()).await.unwrap();
    assert!(matches!(event(&f).await, FrontendEvent::AddEndpoint(_)));

    server.abort();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn bind_unix_replaces_a_stale_socket_but_never_deletes_other_files() {
    let dir = std::env::temp_dir().join(format!("it-matters-bind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("plugins.sock");

    drop(it_matters_core::host::bind_unix(&path).unwrap()); // leaves the socket file behind
    assert!(path.exists());
    drop(it_matters_core::host::bind_unix(&path).unwrap()); // replaced without complaint

    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "precious").unwrap();
    let err = it_matters_core::host::bind_unix(&path).unwrap_err();
    assert!(err.to_string().contains("not a socket"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious");
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- remote agents over TCP -------------------------------------------------------------------------

mod tcp {
    use std::sync::Arc;

    use it_matters_core::host::{Access, serve_tcp};
    use it_matters_model::auth;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::sleep;

    use super::*;

    const TOKEN: &[u8] = b"0123456789abcdef0123456789abcdef";

    struct Agent {
        rd: BufReader<tokio::net::tcp::OwnedReadHalf>,
        wr: tokio::net::tcp::OwnedWriteHalf,
    }

    impl Agent {
        async fn connect(addr: std::net::SocketAddr) -> Agent {
            let (rd, wr) = TcpStream::connect(addr).await.unwrap().into_split();
            Agent { rd: BufReader::new(rd), wr }
        }
        async fn recv(&mut self) -> Option<HostMessage> {
            let mut line = String::new();
            match self.rd.read_line(&mut line).await {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(decode_line(&line).unwrap()),
            }
        }
        async fn nonce(&mut self) -> String {
            match self.recv().await {
                Some(HostMessage::Challenge { nonce }) => nonce,
                other => panic!("expected a challenge first, got {other:?}"),
            }
        }
        async fn send(&mut self, m: &PluginMessage) {
            self.wr.write_all(encode_line(m).unwrap().as_bytes()).await.unwrap();
        }
        async fn hello(&mut self, plugin: &str, proof: Option<String>) {
            self.send(&PluginMessage::Hello { plugin: plugin.into(), version: "t".into(), proto: 1, proof }).await;
        }
        /// A full, correct login.
        async fn login(addr: std::net::SocketAddr, token: &[u8], plugin: &str) -> Agent {
            let mut a = Agent::connect(addr).await;
            let nonce = a.nonce().await;
            a.hello(plugin, Some(auth::proof(token, &nonce, plugin))).await;
            assert_eq!(a.recv().await, Some(HostMessage::Welcome { proto: 1 }));
            a
        }
        async fn expect_auth_failure(&mut self) {
            assert!(
                matches!(self.recv().await, Some(HostMessage::Error { message }) if message == "authentication failed")
            );
            assert_eq!(self.recv().await, None, "the connection is closed after a failure");
        }
    }

    struct Bridge {
        host: HostHandle,
        fake: FakeFrontend,
        addr: std::net::SocketAddr,
        _actor: JoinHandle<()>,
    }

    /// A host with a local plugin `p` and a remote agent `agent`, served by one TCP listener.
    async fn bridge(token: Option<&[u8]>, max_connections: usize) -> Bridge {
        let mut settings = settings();
        settings.plugins.insert("agent".into(), PluginSettings { set_timeout: SET_TIMEOUT });
        settings.remote_plugins.insert("agent".into());
        settings.auth_failure_delay = Duration::from_millis(30);
        let (host_end, frontend_end) = frontend::channel();
        let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
        let (host, actor) = spawn_host(settings, registry, host_end);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_tcp(listener, host.clone(), "agent".into(), token.map(<[u8]>::to_vec), max_connections));
        Bridge { host, fake: FakeFrontend::new(frontend_end), addr, _actor: actor }
    }

    async fn event(b: &Bridge) -> FrontendEvent {
        timeout(Duration::from_secs(3), b.fake.next_event()).await.expect("no event").unwrap()
    }

    #[tokio::test]
    async fn a_correct_proof_logs_the_agent_in_and_its_devices_reach_the_matter_side() {
        let b = bridge(Some(TOKEN), 4).await;
        let mut a = Agent::login(b.addr, TOKEN, "agent").await;
        a.send(&PluginMessage::Register { device: pc() }).await;
        assert!(matches!(event(&b).await, FrontendEvent::AddEndpoint(_)));
    }

    #[tokio::test]
    async fn the_host_speaks_first_with_a_different_nonce_each_time() {
        let b = bridge(Some(TOKEN), 4).await;
        let (mut a1, mut a2) = (Agent::connect(b.addr).await, Agent::connect(b.addr).await);
        let (n1, n2) = (a1.nonce().await, a2.nonce().await);
        assert_ne!(n1, n2);
        assert_eq!(n1.len(), 32);
    }

    #[tokio::test]
    async fn a_wrong_token_a_missing_proof_and_garbage_are_all_refused_the_same_way() {
        let b = bridge(Some(TOKEN), 4).await;
        for proof in [
            Some(auth::proof(b"another-token-another-token-xxxxx", "00", "agent")), // computed with a wrong token (and nonce)
            None,
            Some("not hex".to_owned()),
            Some(String::new()),
        ] {
            let mut a = Agent::connect(b.addr).await;
            a.nonce().await;
            a.hello("agent", proof).await;
            a.expect_auth_failure().await;
        }
        let mut a = Agent::connect(b.addr).await;
        let nonce = a.nonce().await;
        a.hello("agent", Some(auth::proof(b"another-token-another-token-xxxxx", &nonce, "agent"))).await;
        a.expect_auth_failure().await; // the right nonce but the wrong token
    }

    #[tokio::test]
    async fn a_failed_login_is_answered_only_after_a_delay() {
        let b = bridge(Some(TOKEN), 4).await; // the test host uses a 30 ms delay
        let mut a = Agent::connect(b.addr).await;
        a.nonce().await;
        let t = std::time::Instant::now();
        a.hello("agent", None).await;
        a.expect_auth_failure().await;
        assert!(t.elapsed() >= Duration::from_millis(25), "answered too fast: {:?}", t.elapsed());
    }

    #[tokio::test]
    async fn a_proof_recorded_from_another_connection_is_useless() {
        let b = bridge(Some(TOKEN), 4).await;
        let mut first = Agent::connect(b.addr).await;
        let old_nonce = first.nonce().await;
        let recorded = auth::proof(TOKEN, &old_nonce, "agent");
        first.hello("agent", Some(recorded.clone())).await;
        assert_eq!(first.recv().await, Some(HostMessage::Welcome { proto: 1 }), "the genuine login works");
        // An eavesdropper replays the recorded proof on a new connection (which has a new nonce).
        let mut replay = Agent::connect(b.addr).await;
        replay.nonce().await;
        replay.hello("agent", Some(recorded)).await;
        replay.expect_auth_failure().await;
    }

    #[tokio::test]
    async fn a_proof_for_one_plugin_cannot_be_used_for_another() {
        let b = bridge(Some(TOKEN), 4).await;
        // Valid token, but the claimed plugin is a different (local) one: this listener serves `agent` only.
        let mut a = Agent::connect(b.addr).await;
        let nonce = a.nonce().await;
        a.hello("p", Some(auth::proof(TOKEN, &nonce, "p"))).await;
        a.expect_auth_failure().await;
        // And an id that is not configured at all gets the identical answer.
        let mut a = Agent::connect(b.addr).await;
        let nonce = a.nonce().await;
        a.hello("ghost", Some(auth::proof(TOKEN, &nonce, "ghost"))).await;
        a.expect_auth_failure().await;
        // A proof computed for `agent` does not verify for `p`.
        let mut a = Agent::connect(b.addr).await;
        let nonce = a.nonce().await;
        a.hello("p", Some(auth::proof(TOKEN, &nonce, "agent"))).await;
        a.expect_auth_failure().await;
    }

    #[tokio::test]
    async fn a_remote_agents_id_cannot_be_claimed_on_the_local_socket() {
        let b = bridge(Some(TOKEN), 4).await;
        let (end, local) = tokio::io::duplex(1 << 16);
        b.host.accept(local);
        let (rd, mut wr) = tokio::io::split(end);
        wr.write_all(encode_line(&hello("agent")).unwrap().as_bytes()).await.unwrap();
        let mut line = String::new();
        BufReader::new(rd).read_line(&mut line).await.unwrap();
        let msg: HostMessage = decode_line(&line).unwrap();
        assert!(matches!(&msg, HostMessage::Error { message } if message.contains("remote agent")), "{msg:?}");
        // Local plugins still work on the local socket.
        let (end, local) = tokio::io::duplex(1 << 16);
        b.host.accept(local);
        let (rd, mut wr) = tokio::io::split(end);
        wr.write_all(encode_line(&hello("p")).unwrap().as_bytes()).await.unwrap();
        let mut line = String::new();
        BufReader::new(rd).read_line(&mut line).await.unwrap();
        assert_eq!(decode_line::<HostMessage>(&line).unwrap(), HostMessage::Welcome { proto: 1 });
    }

    #[tokio::test]
    async fn a_reconnecting_agent_replaces_its_stale_session_and_keeps_its_endpoints() {
        let b = bridge(Some(TOKEN), 4).await;
        let mut old = Agent::login(b.addr, TOKEN, "agent").await;
        old.send(&PluginMessage::Register { device: pc() }).await;
        assert!(matches!(event(&b).await, FrontendEvent::AddEndpoint(_)));
        // The machine rebooted: a new connection arrives while the host still holds the old one.
        let mut new = Agent::login(b.addr, TOKEN, "agent").await;
        assert_eq!(old.recv().await, None, "the stale session is closed");
        new.send(&PluginMessage::Register { device: pc() }).await;
        assert_eq!(event(&b).await, FrontendEvent::SetReachable { endpoint: 2, reachable: true }, "the same endpoint");
        sleep(Duration::from_millis(100)).await;
        assert!(b.fake.drain().is_empty(), "no unreachable blip and no new endpoint");
    }

    #[tokio::test]
    async fn a_write_in_flight_to_a_replaced_session_fails_instead_of_hanging() {
        let b = bridge(Some(TOKEN), 4).await;
        let mut old = Agent::login(b.addr, TOKEN, "agent").await;
        old.send(&PluginMessage::Register { device: audio() }).await;
        event(&b).await;
        let answer = b.fake.write(2, Write::OnOff(false)).await;
        assert!(matches!(old.recv().await, Some(HostMessage::Set { .. })));
        let _new = Agent::login(b.addr, TOKEN, "agent").await;
        assert_eq!(answer.recv().await.unwrap(), Err("the plugin reconnected".to_owned()));
    }

    #[tokio::test]
    async fn without_a_token_nothing_is_proven_and_a_second_connection_does_not_replace_the_first() {
        let b = bridge(None, 4).await; // a loopback listener without a token
        let mut first = Agent::connect(b.addr).await;
        first.nonce().await;
        first.hello("agent", None).await;
        assert_eq!(first.recv().await, Some(HostMessage::Welcome { proto: 1 }));
        let mut second = Agent::connect(b.addr).await;
        second.nonce().await;
        second.hello("agent", None).await;
        assert!(
            matches!(second.recv().await, Some(HostMessage::Error { message }) if message.contains("already connected"))
        );
        // The id is still pinned to the listener.
        let mut other = Agent::connect(b.addr).await;
        other.nonce().await;
        other.hello("p", None).await;
        other.expect_auth_failure().await;
    }

    #[tokio::test]
    async fn the_number_of_connections_per_listener_is_capped() {
        let b = bridge(Some(TOKEN), 1).await;
        let mut held = Agent::connect(b.addr).await;
        held.nonce().await;
        let mut waiting = Agent::connect(b.addr).await;
        // The second connection is not served (no challenge) while the first holds the only slot.
        assert!(timeout(Duration::from_millis(300), waiting.recv()).await.is_err());
        drop(held);
        waiting.nonce().await; // the slot was freed
    }

    #[tokio::test]
    async fn an_agent_that_goes_silent_is_dropped_by_the_heartbeat_and_its_devices_become_unreachable() {
        // This is how a computer that lost power is noticed: its connection just stops answering.
        let mut s = settings();
        s.plugins.insert("agent".into(), PluginSettings { set_timeout: SET_TIMEOUT });
        s.remote_plugins.insert("agent".into());
        s.ping_interval = Duration::from_millis(40);
        s.idle_timeout = Duration::from_millis(200);
        let (host_end, frontend_end) = frontend::channel();
        let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
        let (host, _actor) = spawn_host(s, registry, host_end);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_tcp(listener, host, "agent".into(), Some(TOKEN.to_vec()), 4));
        let fake = FakeFrontend::new(frontend_end);

        let mut a = Agent::login(addr, TOKEN, "agent").await;
        a.send(&PluginMessage::Register { device: pc() }).await;
        assert!(matches!(
            timeout(Duration::from_secs(2), fake.next_event()).await.unwrap().unwrap(),
            FrontendEvent::AddEndpoint(_)
        ));
        // The agent reads but never answers (a frozen or powered-off machine).
        let drain = async { while a.recv().await.is_some() {} };
        timeout(Duration::from_secs(3), drain).await.expect("the silent agent was never dropped");
        assert_eq!(
            timeout(Duration::from_secs(2), fake.next_event()).await.unwrap().unwrap(),
            FrontendEvent::SetReachable { endpoint: 2, reachable: false }
        );
    }

    #[test]
    fn the_access_policy_for_tcp_is_pinned_to_one_plugin() {
        // Constructing it is part of the public API: a listener is always bound to exactly one plugin.
        let _ = Access::tcp("agent", Some(Arc::new(TOKEN.to_vec())), "1.2.3.4:5");
        let _ = Access::local();
    }
}

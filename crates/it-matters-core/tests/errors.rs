//! The error catalogue in `docs/design/plugin-protocol.md` (section 9) must be exactly what the host says.
//!
//! Every documented message is triggered against the real host, and every message the host sends in these
//! scenarios must be documented: a message that changes, disappears or appears fails here until the docs
//! are updated. (Messages are for people and logs; plugins must not parse them, but they are part of the
//! documented behaviour.)

use std::collections::BTreeMap;
use std::time::Duration;

use it_matters_core::allocator::{Allocator, MemoryStore};
use it_matters_core::frontend;
use it_matters_core::host::{HostHandle, HostSettings, PluginSettings, serve_tcp, spawn_host};
use it_matters_core::registry::Registry;
use it_matters_model::protocol::{HostMessage, MAX_FRAME_BYTES, decode_line};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::net::TcpListener;
use tokio::time::timeout;

const DOC: &str = include_str!("../../../docs/design/plugin-protocol.md");

// ---- reading the catalogue from the documentation ----

/// Every message template in the first column of the tables of section 9.
fn documented_templates() -> Vec<String> {
    let section = DOC.split("## 9. Errors").nth(1).unwrap().split("## 10.").next().unwrap();
    let mut out = Vec::new();
    for line in section.lines().filter(|l| l.starts_with("| `")) {
        let first_cell = line.split('|').nth(1).unwrap();
        out.extend(first_cell.split('`').skip(1).step_by(2).map(str::to_owned));
    }
    out
}

/// Does `actual` fit `template`, where `{name}` stands for any non-empty text? (The whole text must match.)
fn fits(template: &str, actual: &str) -> bool {
    if template.is_empty() {
        return actual.is_empty();
    }
    if let Some(after_open) = template.strip_prefix('{') {
        let close = after_open.find('}').expect("an unclosed placeholder in the documentation");
        let rest = &after_open[close + 1..];
        // The placeholder takes one or more characters; try every length.
        return actual
            .char_indices()
            .skip(1)
            .map(|(i, _)| i)
            .chain([actual.len()])
            .any(|i| i > 0 && fits(rest, &actual[i..]));
    }
    let (t, a) = (template.chars().next().unwrap(), actual.chars().next());
    a == Some(t) && fits(&template[t.len_utf8()..], &actual[t.len_utf8()..])
}

#[test]
fn the_template_matcher_works() {
    assert!(fits("authentication failed", "authentication failed"));
    assert!(!fits("authentication failed", "authentication failed!"));
    assert!(fits("plugin \"{id}\" is not configured", "plugin \"ghost\" is not configured"));
    assert!(!fits("plugin \"{id}\" is not configured", "plugin \"\" is not configured"), "a placeholder is not empty");
    assert!(!fits("plugin \"{id}\" is not configured", "plugin \"x\" is configured"));
    assert!(fits("invalid frame: {reason}", "invalid frame: expected value at line 1"));
    assert!(!fits("invalid frame: {reason}", "invalid frame: "));
    assert!(fits("unsupported protocol {n}; this host speaks 1", "unsupported protocol 99; this host speaks 1"));
    assert!(fits("value {v} is not valid for \"{id}\"/\"{c}\"", "value Int(101) is not valid for \"a\"/\"b\""));
    assert!(!fits("value {v} is not valid for \"{id}\"/\"{c}\"", "value Int(101) is not valid for \"a\""));
    assert!(!fits("a {x} b", "a  b"), "a placeholder is not empty");
    assert!(fits("a {x} b", "a long text b") && !fits("a {x} b", "a long text b c"));
    assert!(fits("{x}", "anything") && !fits("{x}", ""));
}

#[test]
fn the_catalogue_is_not_empty_and_has_no_duplicates() {
    let t = documented_templates();
    assert!(t.len() >= 25, "{t:#?}");
    let mut sorted = t.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), t.len(), "a message is listed twice");
}

// ---- triggering the messages ----

/// Plugin ids: one per scenario, so no scenario depends on another session having ended.
const LOCAL: [&str; 10] = ["p", "q", "o", "i", "v0", "v1", "v2", "v3", "v4", "v5"];

fn settings() -> HostSettings {
    let one = || PluginSettings { set_timeout: Duration::from_secs(2) };
    let mut plugins: BTreeMap<String, PluginSettings> = LOCAL.iter().map(|id| ((*id).to_owned(), one())).collect();
    plugins.insert("agent".into(), one());
    let mut s = HostSettings::new(plugins);
    s.remote_plugins.insert("agent".into());
    s.handshake_timeout = Duration::from_millis(150);
    s.auth_failure_delay = Duration::from_millis(10);
    s
}

struct Conn {
    rd: BufReader<ReadHalf<DuplexStream>>,
    wr: WriteHalf<DuplexStream>,
}

impl Conn {
    fn open(host: &HostHandle) -> Conn {
        let (a, b) = tokio::io::duplex(1 << 20);
        host.accept(b);
        let (rd, wr) = tokio::io::split(a);
        Conn { rd: BufReader::new(rd), wr }
    }
    async fn send(&mut self, bytes: &[u8]) {
        self.wr.write_all(bytes).await.unwrap();
    }
    async fn send_line(&mut self, json: &str) {
        self.send(format!("{json}\n").as_bytes()).await;
    }
    async fn recv(&mut self) -> Option<HostMessage> {
        let mut line = String::new();
        let n = timeout(Duration::from_secs(3), self.rd.read_line(&mut line))
            .await
            .expect("the host did not answer")
            .ok()?;
        (n > 0).then(|| decode_line(&line).unwrap())
    }
    /// The text of the next `error`, skipping heartbeats.
    async fn error(&mut self) -> String {
        loop {
            match self.recv().await {
                Some(HostMessage::Error { message }) => return message,
                Some(HostMessage::Ping) => continue,
                other => panic!("expected an error, got {other:?}"),
            }
        }
    }
    async fn hello(&mut self, plugin: &str) {
        self.send_line(&format!(r#"{{"type":"hello","plugin":"{plugin}","version":"t","proto":1}}"#)).await;
    }
    async fn login(host: &HostHandle, plugin: &str) -> Conn {
        let mut c = Conn::open(host);
        c.hello(plugin).await;
        assert_eq!(c.recv().await, Some(HostMessage::Welcome { proto: 1 }));
        c
    }
    async fn closed(&mut self) -> bool {
        loop {
            match self.recv().await {
                None => return true,
                Some(HostMessage::Ping) => continue,
                Some(_) => return false,
            }
        }
    }
}

fn level(id: &str, writable: bool, min: i64, max: i64) -> String {
    format!(r#"{{"id":"{id}","type":"level","min":{min},"max":{max},"writable":{writable}}}"#)
}
fn boolean(id: &str, writable: bool, role: Option<&str>) -> String {
    let role = role.map_or(String::new(), |r| format!(r#","role":"{r}""#));
    format!(r#"{{"id":"{id}","type":"boolean","writable":{writable}{role}}}"#)
}
fn register(id: &str, name: &str, caps: &[String]) -> String {
    format!(r#"{{"type":"register","device":{{"id":"{id}","name":"{name}","capabilities":[{}]}}}}"#, caps.join(","))
}

/// Every error message the host sends in these scenarios.
async fn trigger_everything() -> Vec<String> {
    let (host_end, _frontend_end) = frontend::channel();
    let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
    let (host, _actor) = spawn_host(settings(), registry, host_end);
    let mut got = Vec::new();

    // ---- the host closes the connection afterwards ----
    let mut c = Conn::open(&host); // says nothing at all
    got.push(c.error().await);
    assert!(c.closed().await);

    let mut c = Conn::open(&host);
    c.send_line(r#"{"type":"ping"}"#).await;
    got.push(c.error().await);
    assert!(c.closed().await);

    let mut c = Conn::open(&host);
    c.send_line(r#"{"type":"hello","plugin":"p","version":"t","proto":99}"#).await;
    got.push(c.error().await);
    assert!(c.closed().await);

    let mut c = Conn::open(&host);
    c.hello("ghost").await;
    got.push(c.error().await);
    assert!(c.closed().await);

    let mut c = Conn::open(&host);
    c.hello("agent").await;
    got.push(c.error().await);
    assert!(c.closed().await);

    let _holds_p = Conn::login(&host, "p").await;
    let mut second = Conn::open(&host);
    second.hello("p").await;
    got.push(second.error().await);
    assert!(second.closed().await);

    let mut c = Conn::login(&host, "q").await;
    c.hello("q").await;
    got.push(c.error().await);
    assert!(c.closed().await);

    // Protocol violations after a good handshake, each on its own plugin id.
    let violations: [(&str, Vec<u8>); 5] = [
        ("v0", [vec![b'x'; MAX_FRAME_BYTES + 10], b"\n".to_vec()].concat()),
        ("v1", b"this is not json\n".to_vec()),
        ("v2", b"{\"type\":\"explode\"}\n".to_vec()),
        ("v3", b"\n".to_vec()),
        ("v4", vec![0xff, 0xfe, b'\n']),
    ];
    for (id, bytes) in violations {
        let mut c = Conn::login(&host, id).await;
        c.send(&bytes).await;
        got.push(c.error().await);
        assert!(c.closed().await, "the connection must be closed after a protocol violation");
    }

    // A remote agent that fails to authenticate (TCP; the listener requires a token).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_tcp(
        listener,
        host.clone(),
        "agent".into(),
        Some(b"0123456789abcdef0123456789abcdef".to_vec()),
        4,
    ));
    let (rd, mut wr) = tokio::net::TcpStream::connect(addr).await.unwrap().into_split();
    let mut rd = BufReader::new(rd).lines();
    assert!(rd.next_line().await.unwrap().unwrap().contains("\"challenge\""));
    wr.write_all(b"{\"type\":\"hello\",\"plugin\":\"agent\",\"version\":\"t\",\"proto\":1}\n").await.unwrap();
    match decode_line::<HostMessage>(&rd.next_line().await.unwrap().unwrap()).unwrap() {
        HostMessage::Error { message } => got.push(message),
        other => panic!("expected an error, got {other:?}"),
    }

    // ---- the connection stays open ----
    let mut owner = Conn::login(&host, "o").await;
    let bad_registrations = [
        register("Bad Id", "n", &[boolean("a", false, None)]),
        register("d", "  ", &[boolean("a", false, None)]),
        register("d", "n", &[]),
        register("d", "n", &[boolean("Bad Cap", false, None)]),
        register("d", "n", &[boolean("a", false, None), boolean("a", false, None)]),
        register("d", "n", &[level("v", true, 5, 5), boolean("m", true, Some("mute"))]),
        register("d", "n", &[level("a", true, 0, 9), level("b", true, 0, 9), boolean("m", true, Some("mute"))]),
        register(
            "d",
            "n",
            &[level("v", true, 0, 9), boolean("m", true, Some("mute")), boolean("n", true, Some("mute"))],
        ),
        register("d", "n", &[level("v", false, 0, 9), boolean("m", true, Some("mute"))]),
        register("d", "n", &[level("v", true, 0, 9)]),
        register("d", "n", &[boolean("m", true, Some("mute"))]),
        register("d", "n", &[boolean("x", true, Some("occupancy"))]),
        register("d", "n", &[boolean("x", true, Some("contact"))]),
    ];
    for r in bad_registrations {
        owner.send_line(&r).await;
        got.push(owner.error().await);
    }

    // `o` owns the device `pc`; `i` is another plugin.
    owner
        .send_line(&register(
            "pc",
            "PC",
            &[boolean("online", false, None), level("vol", true, 0, 100), boolean("mute", true, Some("mute"))],
        ))
        .await;
    for bad_update in [
        r#"{"type":"update","device":"pc","capability":"nope","value":true}"#,
        r#"{"type":"update","device":"pc","capability":"vol","value":101}"#,
        r#"{"type":"update","device":"ghost","capability":"x","value":true}"#,
        r#"{"type":"unregister","device":"ghost"}"#,
    ] {
        owner.send_line(bad_update).await;
        got.push(owner.error().await);
    }
    let mut intruder = Conn::login(&host, "i").await;
    for attempt in [
        register("pc", "PC", &[boolean("online", false, None)]),
        r#"{"type":"update","device":"pc","capability":"online","value":true}"#.to_owned(),
        r#"{"type":"unregister","device":"pc"}"#.to_owned(),
    ] {
        intruder.send_line(&attempt).await;
        got.push(intruder.error().await);
    }
    // The connection really did stay open through all of the above.
    owner.send_line(r#"{"type":"ping"}"#).await;
    assert_eq!(owner.recv().await, Some(HostMessage::Pong));
    got
}

#[tokio::test]
async fn every_documented_error_is_what_the_host_says() {
    let actual = trigger_everything().await;
    let templates = documented_templates();
    for t in &templates {
        assert!(
            actual.iter().any(|a| fits(t, a)),
            "documented but the host never says it: {t}\nthe host said: {actual:#?}"
        );
    }
    for a in &actual {
        assert!(templates.iter().any(|t| fits(t, a)), "the host says something undocumented: {a:?}");
    }
}

//! A client library for writing it-matters plugins in Rust (any other language can speak the
//! [protocol](https://github.com/gregbacchus/it-matters/blob/main/docs/design/plugin-protocol.md)
//! directly; see `examples/plugins/demo.py`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use it_matters_plugin_sdk::{Env, Event, connect};
//! let env = Env::from_env()?;                                   // set by the daemon that starts the plugin
//! let (client, mut events) = connect(&env.socket, &env.plugin_id, "0.1.0").await?;
//! // client.register(device).await?; client.update("dev", "cap", Value::Bool(true)).await?;
//! while let Some(event) = events.next().await {
//!     match event {
//!         Event::Set(request) => request.accept().await,       // a controller wrote a capability
//!         Event::HostError(message) => eprintln!("host: {message}"),
//!         Event::Closed(reason) => return Err(reason.into()),   // exit: the supervisor restarts us
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! The SDK performs the handshake, answers the host's heartbeat pings itself, and caps incoming lines.
//! When the connection ends it reports [`Event::Closed`]; a plugin should then exit, because the daemon's
//! supervisor restarts it and it registers from scratch.

use std::path::{Path, PathBuf};
use std::time::Duration;

use it_matters_model::protocol::{HostMessage, MAX_FRAME_BYTES, PluginMessage, decode_line, encode_line};
use it_matters_model::{DeviceSpec, PROTO_VERSION, Value, valid_id};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// How long the host gets to answer `hello`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the daemon tells a plugin it started: where to connect and which plugin it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub socket: PathBuf,
    pub plugin_id: String,
}

impl Env {
    /// Reads `IT_MATTERS_SOCKET` and `IT_MATTERS_PLUGIN_ID`.
    pub fn from_env() -> Result<Self, Error> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Like [`Env::from_env`], with the lookup injected (for tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let need = |k: &str| {
            get(k).filter(|v| !v.is_empty()).ok_or_else(|| {
                Error::Env(format!("{k} is not set; the it-matters daemon sets it when it starts a plugin (`run`)"))
            })
        };
        Ok(Self { socket: PathBuf::from(need("IT_MATTERS_SOCKET")?), plugin_id: need("IT_MATTERS_PLUGIN_ID")? })
    }
}

#[derive(Debug)]
pub enum Error {
    Env(String),
    Io(std::io::Error),
    /// The host refused the connection (for example the plugin id is not configured).
    Refused(String),
    /// The host sent something unexpected during the handshake.
    Protocol(String),
    /// The connection is gone.
    Closed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Env(m) => f.write_str(m),
            Error::Io(e) => write!(f, "{e}"),
            Error::Refused(m) => write!(f, "the host refused the connection: {m}"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
            Error::Closed => f.write_str("the connection to the host is closed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Sends messages to the host. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    out: mpsc::Sender<PluginMessage>,
}

impl Client {
    async fn send(&self, m: PluginMessage) -> Result<(), Error> {
        self.out.send(m).await.map_err(|_| Error::Closed)
    }

    /// Registers a device (also the way to re-register after a reconnect). The host rejects
    /// unmappable devices with an [`Event::HostError`]. Send the current values right afterwards.
    pub async fn register(&self, device: DeviceSpec) -> Result<(), Error> {
        self.send(PluginMessage::Register { device }).await
    }

    pub async fn unregister(&self, device: &str) -> Result<(), Error> {
        self.send(PluginMessage::Unregister { device: device.to_owned() }).await
    }

    /// Reports a capability's current value.
    pub async fn update(&self, device: &str, capability: &str, value: Value) -> Result<(), Error> {
        self.send(PluginMessage::Update { device: device.to_owned(), capability: capability.to_owned(), value }).await
    }
}

/// A controller's write that the plugin must answer, with [`SetRequest::accept`] or
/// [`SetRequest::refuse`], within the plugin's `timeout_ms`.
#[must_use = "the controller waits for an answer: call accept() or refuse()"]
pub struct SetRequest {
    pub device: String,
    pub capability: String,
    pub value: Value,
    req_id: u64,
    out: mpsc::Sender<PluginMessage>,
}

impl std::fmt::Debug for SetRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SetRequest({}/{} = {:?})", self.device, self.capability, self.value)
    }
}

impl SetRequest {
    pub async fn accept(self) {
        let _ = self.out.send(PluginMessage::SetResult { req_id: self.req_id, ok: true, error: None }).await;
    }

    pub async fn refuse(self, reason: impl Into<String>) {
        let m = PluginMessage::SetResult { req_id: self.req_id, ok: false, error: Some(reason.into()) };
        let _ = self.out.send(m).await;
    }
}

#[derive(Debug)]
pub enum Event {
    /// A controller wrote a writable capability.
    Set(SetRequest),
    /// The host rejected something the plugin sent (the connection stays open).
    HostError(String),
    /// The connection ended. The plugin should exit and be restarted by the daemon.
    Closed(String),
}

/// Incoming events from the host.
pub struct Events {
    rx: mpsc::Receiver<Event>,
}

impl Events {
    /// The next event; `None` once [`Event::Closed`] has been delivered.
    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

/// Reads one `\n`-terminated line, never buffering more than [`MAX_FRAME_BYTES`].
async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut buf = Vec::new();
    loop {
        let (used, done) = {
            let chunk = r.fill_buf().await.map_err(|e| e.to_string())?;
            if chunk.is_empty() {
                return Ok(if buf.is_empty() { None } else { Some(buf) });
            }
            let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => (i, true),
                None => (chunk.len(), false),
            };
            if buf.len() + take > MAX_FRAME_BYTES {
                return Err("the host sent a line that is too long".into());
            }
            buf.extend_from_slice(&chunk[..take]);
            (take + usize::from(done), done)
        };
        r.consume(used);
        if done {
            return Ok(Some(buf));
        }
    }
}

fn parse(frame: &[u8]) -> Result<HostMessage, String> {
    let text = std::str::from_utf8(frame).map_err(|_| "the host sent invalid UTF-8".to_owned())?;
    decode_line(text).map_err(|e| e.to_string())
}

/// Connects to the daemon's plugin socket and performs the handshake.
pub async fn connect(socket: &Path, plugin_id: &str, version: &str) -> Result<(Client, Events), Error> {
    let stream = UnixStream::connect(socket).await.map_err(|e| {
        Error::Io(std::io::Error::new(e.kind(), format!("cannot connect to {}: {e}", socket.display())))
    })?;
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);

    let hello = PluginMessage::Hello {
        plugin: plugin_id.to_owned(),
        version: version.to_owned(),
        proto: PROTO_VERSION,
        token: None,
    };
    wr.write_all(encode_line(&hello).map_err(|e| Error::Protocol(e.to_string()))?.as_bytes()).await?;
    let first = timeout(HANDSHAKE_TIMEOUT, read_line(&mut rd))
        .await
        .map_err(|_| Error::Protocol("the host did not answer hello in time".into()))?
        .map_err(Error::Protocol)?
        .ok_or(Error::Closed)?;
    match parse(&first).map_err(Error::Protocol)? {
        HostMessage::Welcome { .. } => {}
        HostMessage::Error { message } => return Err(Error::Refused(message)),
        other => return Err(Error::Protocol(format!("expected welcome, got {other:?}"))),
    }

    let (out_tx, mut out_rx) = mpsc::channel::<PluginMessage>(64);
    let (ev_tx, ev_rx) = mpsc::channel::<Event>(64);

    // Writer: serialises everything the plugin and the reader want to send.
    tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let Ok(line) = encode_line(&m) else { continue };
            if wr.write_all(line.as_bytes()).await.is_err() || wr.flush().await.is_err() {
                break;
            }
        }
    });
    // Reader: answers pings, turns the rest into events.
    tokio::spawn(read_task(rd, out_tx.clone(), ev_tx));

    Ok((Client { out: out_tx }, Events { rx: ev_rx }))
}

async fn read_task<R: AsyncRead + Unpin>(rd: BufReader<R>, out: mpsc::Sender<PluginMessage>, ev: mpsc::Sender<Event>) {
    let mut rd = rd;
    let reason = loop {
        let frame = match read_line(&mut rd).await {
            Ok(Some(f)) => f,
            Ok(None) => break "the host closed the connection".to_owned(),
            Err(e) => break e,
        };
        let event = match parse(&frame) {
            Ok(HostMessage::Ping) => {
                let _ = out.send(PluginMessage::Pong).await;
                continue;
            }
            Ok(HostMessage::Pong | HostMessage::Welcome { .. }) => continue,
            Ok(HostMessage::Set { req_id, device, capability, value }) => {
                Event::Set(SetRequest { device, capability, value, req_id, out: out.clone() })
            }
            Ok(HostMessage::Error { message }) => Event::HostError(message),
            Err(e) => break e,
        };
        if ev.send(event).await.is_err() {
            return;
        }
    };
    let _ = ev.send(Event::Closed(reason)).await;
}

/// Turns any text into a valid device/capability id: lower case, anything else becomes `-`, trimmed,
/// at most 64 characters; `fallback` if nothing is left. Used for default ids built from host names.
pub fn sanitize_id(text: &str, fallback: &str) -> String {
    let mapped: String = text
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' { c } else { '-' })
        .collect();
    let mut id: String = mapped.trim_matches('-').chars().take(64).collect();
    id = id.trim_matches('-').to_owned();
    if valid_id(&id) { id } else { fallback.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_needs_both_variables() {
        let ok = Env::from_lookup(|k| Some(format!("v-{k}"))).unwrap();
        assert_eq!(
            (ok.socket, ok.plugin_id.as_str()),
            (PathBuf::from("v-IT_MATTERS_SOCKET"), "v-IT_MATTERS_PLUGIN_ID")
        );
        let e = Env::from_lookup(|k| (k == "IT_MATTERS_PLUGIN_ID").then(|| "x".to_owned())).unwrap_err();
        assert!(e.to_string().contains("IT_MATTERS_SOCKET is not set"), "{e}");
        assert!(Env::from_lookup(|_| Some(String::new())).is_err(), "empty counts as unset");
    }

    #[test]
    fn ids_are_sanitised() {
        assert_eq!(sanitize_id("Office PC", "pc"), "office-pc");
        assert_eq!(sanitize_id("  --kaizen.local--", "pc"), "kaizen-local");
        assert_eq!(sanitize_id("ÄÖ", "pc"), "pc");
        assert_eq!(sanitize_id("", "pc"), "pc");
        assert_eq!(sanitize_id(&"a".repeat(100), "pc").len(), 64);
        for s in ["Office PC", "x_y", "A-B", "..", "9"] {
            assert!(valid_id(&sanitize_id(s, "pc")), "{s}");
        }
    }
}

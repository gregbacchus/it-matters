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
use it_matters_model::{DeviceSpec, PROTO_VERSION, Value, auth, valid_id};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
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
            Error::Refused(m) if m == "authentication failed" => write!(
                f,
                "the host refused the connection: authentication failed (check the token file and that --plugin-id is the id the bridge configured for this agent)"
            ),
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

/// Connects to the daemon's local plugin socket and performs the handshake.
pub async fn connect(socket: &Path, plugin_id: &str, version: &str) -> Result<(Client, Events), Error> {
    let stream = UnixStream::connect(socket).await.map_err(|e| {
        Error::Io(std::io::Error::new(e.kind(), format!("cannot connect to {}: {e}", socket.display())))
    })?;
    let (rd, wr) = stream.into_split();
    handshake(BufReader::new(rd), wr, plugin_id, version, None).await
}

/// Connects to a bridge's TCP listener for this agent (`plugins[].connect` in the daemon's configuration).
///
/// The host speaks first with a random challenge; with a `token` the agent answers with a proof computed from
/// it (the token itself is never sent). Without a token only a listener that requires none accepts the agent.
/// The connection is **not encrypted**: use a trusted network or a tunnel.
pub async fn connect_tcp(
    addr: &str,
    plugin_id: &str,
    version: &str,
    token: Option<&str>,
) -> Result<(Client, Events), Error> {
    let stream = timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("cannot connect to {addr}: timed out")))
        })?
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("cannot connect to {addr}: {e}"))))?;
    let _ = stream.set_nodelay(true);
    let (rd, wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let first = timeout(HANDSHAKE_TIMEOUT, read_line(&mut rd))
        .await
        .map_err(|_| {
            Error::Protocol(
                "the host did not send a challenge in time (is this the agent port of an it-matters bridge?)".into(),
            )
        })?
        .map_err(Error::Protocol)?
        .ok_or(Error::Closed)?;
    let proof = match parse(&first).map_err(Error::Protocol)? {
        HostMessage::Challenge { nonce } => token.map(|t| auth::proof(t.as_bytes(), &nonce, plugin_id)),
        HostMessage::Error { message } => return Err(Error::Refused(message)),
        other => return Err(Error::Protocol(format!("expected a challenge, got {other:?}"))),
    };
    handshake(rd, wr, plugin_id, version, proof).await
}

/// Sends `hello` and waits for `welcome`, then starts the reader and writer tasks.
async fn handshake<R, W>(
    mut rd: BufReader<R>,
    mut wr: W,
    plugin_id: &str,
    version: &str,
    proof: Option<String>,
) -> Result<(Client, Events), Error>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let hello =
        PluginMessage::Hello { plugin: plugin_id.to_owned(), version: version.to_owned(), proto: PROTO_VERSION, proof };
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
            Ok(HostMessage::Pong | HostMessage::Welcome { .. } | HostMessage::Challenge { .. }) => continue,
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

/// Where a plugin connects to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The daemon's local socket, as a plugin the daemon started does. If the connection ends the plugin
    /// should exit: the daemon's supervisor restarts it.
    Local { socket: PathBuf, plugin_id: String },
    /// A bridge's TCP listener for this agent, from another machine. Nobody restarts an agent, so it
    /// should reconnect by itself (see [`Connector`]).
    Remote { addr: String, plugin_id: String, token: Option<String> },
}

impl Target {
    pub fn plugin_id(&self) -> &str {
        match self {
            Target::Local { plugin_id, .. } | Target::Remote { plugin_id, .. } => plugin_id,
        }
    }

    /// Whether a lost connection should be retried instead of ending the process.
    pub fn reconnects(&self) -> bool {
        matches!(self, Target::Remote { .. })
    }
}

/// The command line flags every plugin accepts to run as a remote agent. Plugins document them on their pages.
pub const REMOTE_FLAGS: [&str; 3] = ["--connect", "--token-file", "--plugin-id"];

/// The remote-agent flags of a plugin's command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteOptions {
    /// `--connect host:port`
    pub connect: Option<String>,
    /// `--token-file path`
    pub token_file: Option<PathBuf>,
    /// `--plugin-id id`
    pub plugin_id: Option<String>,
}

impl RemoteOptions {
    /// Stores the value of one of [`REMOTE_FLAGS`].
    pub fn set(&mut self, flag: &str, value: String) -> Result<(), String> {
        match flag {
            "--connect" => {
                if !value.contains(':') || value.starts_with(':') || value.ends_with(':') {
                    return Err(format!("--connect {value:?} must be host:port, for example 192.168.1.10:7550"));
                }
                self.connect = Some(value);
            }
            "--token-file" => self.token_file = Some(PathBuf::from(value)),
            "--plugin-id" => {
                if !valid_id(&value) {
                    return Err(format!("--plugin-id {value:?} must be 1-64 characters of a-z, 0-9, '-' or '_'"));
                }
                self.plugin_id = Some(value);
            }
            other => return Err(format!("{other} is not a remote-agent flag")),
        }
        Ok(())
    }

    /// Decides where to connect. Without `--connect` the plugin was started by the daemon and the
    /// environment variables it set say where to connect; with it, `--plugin-id` (or
    /// `IT_MATTERS_PLUGIN_ID`) names the plugin and `--token-file` supplies the token (read here; a token
    /// file that other users can read gets a warning on standard error).
    pub fn into_target(self, env: impl Fn(&str) -> Option<String>) -> Result<Target, Error> {
        let Some(addr) = self.connect else {
            if self.token_file.is_some() || self.plugin_id.is_some() {
                return Err(Error::Env("--token-file and --plugin-id only make sense together with --connect".into()));
            }
            let e = Env::from_lookup(env)?;
            return Ok(Target::Local { socket: e.socket, plugin_id: e.plugin_id });
        };
        let plugin_id =
            self.plugin_id.or_else(|| env("IT_MATTERS_PLUGIN_ID").filter(|v| !v.is_empty())).ok_or_else(|| {
                Error::Env("--plugin-id is required with --connect: the id the bridge configured for this agent".into())
            })?;
        let token = match self.token_file {
            None => None,
            Some(path) => {
                let file = auth::read_token_file(&path).map_err(Error::Env)?;
                if file.too_open {
                    eprintln!("warning: {} can be read by other users; chmod 600 it", path.display());
                }
                Some(file.token)
            }
        };
        Ok(Target::Remote { addr, plugin_id, token })
    }
}

/// How long to wait before reconnect attempt number `attempt` (0 is the first retry): 1 s, doubling, at
/// most 30 s.
pub fn reconnect_delay(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(5)).min(Duration::from_secs(30))
}

/// Connects to a [`Target`], and for a remote one keeps coming back with a growing delay.
///
/// ```ignore
/// let mut connector = Connector::new(target, VERSION);
/// loop {
///     match connector.connect().await {
///         Ok((client, mut events)) => { /* register, serve until Event::Closed ... */ connector.reset(); }
///         Err(e) => eprintln!("{e}"),
///     }
///     if !connector.reconnects() { break; } // local: exit, the daemon restarts us
///     connector.wait().await;
/// }
/// ```
pub struct Connector {
    target: Target,
    version: String,
    attempt: u32,
}

impl Connector {
    pub fn new(target: Target, version: &str) -> Self {
        Self { target, version: version.to_owned(), attempt: 0 }
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn reconnects(&self) -> bool {
        self.target.reconnects()
    }

    /// One connection attempt.
    pub async fn connect(&mut self) -> Result<(Client, Events), Error> {
        match &self.target {
            Target::Local { socket, plugin_id } => connect(socket, plugin_id, &self.version).await,
            Target::Remote { addr, plugin_id, token } => {
                connect_tcp(addr, plugin_id, &self.version, token.as_deref()).await
            }
        }
    }

    /// Call after a successful connection so that the next outage starts again with a short delay.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Sleeps for the next reconnect delay.
    pub async fn wait(&mut self) {
        tokio::time::sleep(reconnect_delay(self.attempt)).await;
        self.attempt = self.attempt.saturating_add(1);
    }
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
    fn reconnect_delays_grow_to_a_cap() {
        let d: Vec<u64> = (0..8).map(|a| reconnect_delay(a).as_secs()).collect();
        assert_eq!(d, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(reconnect_delay(u32::MAX).as_secs(), 30);
    }

    fn env_with_plugin_id(k: &str) -> Option<String> {
        (k == "IT_MATTERS_PLUGIN_ID").then(|| "from-env".to_owned())
    }

    #[test]
    fn remote_flags_are_validated() {
        let mut o = RemoteOptions::default();
        assert!(o.set("--connect", "nohost".into()).is_err());
        assert!(o.set("--connect", ":7550".into()).is_err());
        assert!(o.set("--connect", "bridge:".into()).is_err());
        assert!(o.set("--plugin-id", "Bad Id".into()).is_err());
        assert!(o.set("--bogus", "x".into()).is_err());
        o.set("--connect", "192.168.1.10:7550".into()).unwrap();
        o.set("--plugin-id", "desktop".into()).unwrap();
        assert_eq!((o.connect.as_deref(), o.plugin_id.as_deref()), (Some("192.168.1.10:7550"), Some("desktop")));
    }

    #[test]
    fn without_connect_the_daemons_environment_decides() {
        let env = |k: &str| Some(format!("v-{k}"));
        let t = RemoteOptions::default().into_target(env).unwrap();
        assert_eq!(
            t,
            Target::Local { socket: PathBuf::from("v-IT_MATTERS_SOCKET"), plugin_id: "v-IT_MATTERS_PLUGIN_ID".into() }
        );
        assert!(!t.reconnects());
        let stray = RemoteOptions { plugin_id: Some("x".into()), ..Default::default() };
        assert!(stray.into_target(env).unwrap_err().to_string().contains("only make sense together with --connect"));
    }

    #[test]
    fn a_remote_target_needs_a_plugin_id_and_reads_the_token() {
        let missing = RemoteOptions { connect: Some("h:1".into()), ..Default::default() };
        assert!(missing.into_target(|_| None).unwrap_err().to_string().contains("--plugin-id is required"));
        let from_env = RemoteOptions { connect: Some("h:1".into()), ..Default::default() };
        let t = from_env.into_target(env_with_plugin_id).unwrap();
        assert_eq!(t, Target::Remote { addr: "h:1".into(), plugin_id: "from-env".into(), token: None });
        assert!(t.reconnects() && t.plugin_id() == "from-env");

        let dir = std::env::temp_dir().join(format!("it-matters-sdk-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t");
        std::fs::write(&file, format!("{}\n", "ab".repeat(16))).unwrap();
        let o =
            RemoteOptions { connect: Some("h:1".into()), token_file: Some(file.clone()), plugin_id: Some("p".into()) };
        let Target::Remote { token, .. } = o.into_target(|_| None).unwrap() else { panic!() };
        assert_eq!(token.as_deref(), Some("ab".repeat(16).as_str()));
        std::fs::write(&file, "short").unwrap();
        let o = RemoteOptions { connect: Some("h:1".into()), token_file: Some(file), plugin_id: Some("p".into()) };
        assert!(o.into_target(|_| None).unwrap_err().to_string().contains("too short"));
        std::fs::remove_dir_all(&dir).unwrap();
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

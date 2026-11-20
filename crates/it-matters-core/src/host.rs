//! The plugin host: accepts plugin connections, speaks the wire protocol, and drives the
//! [`Registry`] and the frontend channels.
//!
//! Structure: one **actor** task owns the registry and all cross-plugin state, so every change and
//! the [`FrontendEvent`]s it causes happen in one total order and no lock is held across an await.
//! Each connection is a **session** task (generic over the stream: a Unix socket, TCP later, or an
//! in-memory pipe in tests) that frames lines, enforces the handshake and heartbeat, and forwards
//! messages to the actor.
//!
//! Rules enforced here:
//! * the first message must be `hello` within `handshake_timeout`, with a supported `proto` and a
//!   `plugin` id that is listed in the configuration (unlisted plugins are refused);
//! * one live session per plugin id: a second connection is refused while the first is alive
//!   (the plugin SDK retries with backoff; a dead connection is noticed on EOF or by the heartbeat);
//! * frames are limited to [`MAX_FRAME_BYTES`]; an oversized or malformed frame closes the session;
//! * the host pings every `ping_interval` and closes a session that sent nothing for `idle_timeout`;
//! * a controller write is forwarded as `set`; the plugin must answer within its `set_timeout`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use it_matters_model::protocol::{
    FrameError, HostMessage, MAX_FRAME_BYTES, PluginMessage, decode_line, encode_line, proto_compatible,
};
use it_matters_model::{PROTO_VERSION, auth};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, interval_at, sleep, sleep_until, timeout};

use crate::allocator::Store;
use crate::frontend::{ControllerCommand, FrontendEvent, HostEnd};
use crate::registry::{Registry, Route};

/// Per-plugin settings.
#[derive(Debug, Clone)]
pub struct PluginSettings {
    /// How long the plugin has to answer a `set`.
    pub set_timeout: Duration,
}

/// Host settings. `plugins` is the allow-list: only these ids may connect.
#[derive(Debug, Clone)]
pub struct HostSettings {
    pub plugins: BTreeMap<String, PluginSettings>,
    /// Plugins that are remote agents: they may only connect through their own TCP listener, never over
    /// the local Unix socket (where they would not need to prove anything).
    pub remote_plugins: BTreeSet<String>,
    pub handshake_timeout: Duration,
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
    /// How long a failed authentication waits before it is answered (slows guessing).
    pub auth_failure_delay: Duration,
}

impl HostSettings {
    pub fn new(plugins: BTreeMap<String, PluginSettings>) -> Self {
        Self {
            plugins,
            remote_plugins: BTreeSet::new(),
            handshake_timeout: Duration::from_secs(5),
            ping_interval: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            auth_failure_delay: Duration::from_millis(500),
        }
    }
}

enum Input {
    Connect {
        plugin: String,
        out: mpsc::Sender<HostMessage>,
        /// An authenticated remote agent replaces a session that is still registered (almost certainly a
        /// half-open connection from before a reboot or a network drop).
        replace: bool,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Message {
        plugin: String,
        session: u64,
        msg: PluginMessage,
    },
    Gone {
        plugin: String,
        session: u64,
    },
}

/// A cloneable handle for handing connections to the host.
#[derive(Clone)]
pub struct HostHandle {
    input: mpsc::Sender<Input>,
    settings: Arc<HostSettings>,
}

/// Starts the host actor. It stops when every [`HostHandle`] has been dropped.
pub fn spawn_host<S: Store + Send + 'static>(
    settings: HostSettings,
    registry: Registry<S>,
    frontend: HostEnd,
) -> (HostHandle, JoinHandle<()>) {
    let settings = Arc::new(settings);
    let (input, rx) = mpsc::channel(256);
    let actor = Actor {
        registry,
        settings: settings.clone(),
        frontend,
        sessions: HashMap::new(),
        pending: BTreeMap::new(),
        next_session: 1,
        next_req: 1,
        commands_open: true,
    };
    (HostHandle { input, settings }, tokio::spawn(actor.run(rx)))
}

/// How a connection is authenticated and which plugin it may be.
#[derive(Clone)]
pub struct Access {
    /// A remote (TCP) connection: the host speaks first with a challenge, and only the listener's own
    /// plugin may connect.
    remote: bool,
    only_plugin: Option<String>,
    /// The listener's token; `None` means no proof is required (a loopback listener without a token).
    token: Option<Arc<Vec<u8>>>,
    /// For logs only.
    peer: String,
}

impl Access {
    /// The local Unix socket: any configured, non-remote plugin; trust comes from the socket's permissions.
    pub fn local() -> Self {
        Self { remote: false, only_plugin: None, token: None, peer: "local socket".into() }
    }

    /// A TCP listener that serves exactly one plugin.
    pub fn tcp(plugin: &str, token: Option<Arc<Vec<u8>>>, peer: impl Into<String>) -> Self {
        Self { remote: true, only_plugin: Some(plugin.to_owned()), token, peer: peer.into() }
    }
}

impl HostHandle {
    /// Runs one local connection. The returned task ends when the session does.
    pub fn accept<T>(&self, stream: T) -> JoinHandle<()>
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        self.accept_with(stream, Access::local())
    }

    /// Runs one connection with the given access policy.
    pub fn accept_with<T>(&self, stream: T, access: Access) -> JoinHandle<()>
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        tokio::spawn(run_session(stream, self.clone(), access))
    }
}

// -------------------------------------------------------------------------------------------------
// TCP
// -------------------------------------------------------------------------------------------------

/// The most agent connections one TCP listener serves at once.
pub const MAX_TCP_CONNECTIONS: usize = 16;

/// Accepts remote agent connections for **one** plugin until the task is dropped. With a `token` the agent
/// must prove it knows it; the token is the identity, so a second authenticated connection for the same
/// plugin replaces the first. Without a token (only sensible on a loopback address) anyone who can reach the
/// port is the plugin, and a second connection is refused while the first lives.
pub async fn serve_tcp(
    listener: tokio::net::TcpListener,
    host: HostHandle,
    plugin: String,
    token: Option<Vec<u8>>,
    max_connections: usize,
) {
    let token = token.map(Arc::new);
    let slots = Arc::new(tokio::sync::Semaphore::new(max_connections));
    loop {
        let Ok(permit) = slots.clone().acquire_owned().await else { return };
        match listener.accept().await {
            Ok((stream, peer)) => {
                let _ = stream.set_nodelay(true);
                let session = host.accept_with(stream, Access::tcp(&plugin, token.clone(), peer.to_string()));
                tokio::spawn(async move {
                    let _ = session.await;
                    drop(permit);
                });
            }
            Err(_) => {
                drop(permit);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Unix socket
// -------------------------------------------------------------------------------------------------

/// The most plugin connections served at once; further ones wait to be accepted.
pub const MAX_CONNECTIONS: usize = 64;

/// Binds the plugin socket: creates its directory, replaces a stale *socket* left by a previous run
/// (but refuses to delete anything else that sits at the path) and restricts access to the owner
/// and group (`0660`).
pub fn bind_unix(path: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

/// Accepts plugin connections until the task is dropped.
pub async fn serve_unix(listener: tokio::net::UnixListener, host: HostHandle) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok(permit) = slots.clone().acquire_owned().await else { return };
        match listener.accept().await {
            Ok((stream, _)) => {
                let session = host.accept(stream);
                tokio::spawn(async move {
                    let _ = session.await;
                    drop(permit);
                });
            }
            Err(_) => {
                drop(permit);
                // e.g. out of file descriptors: do not spin.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Framing
// -------------------------------------------------------------------------------------------------

#[derive(Debug)]
enum Inbound {
    Frame(Vec<u8>),
    TooLong,
    Eof,
    Io,
}

/// Reads one `\n`-terminated frame, never buffering more than [`MAX_FRAME_BYTES`].
async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> Inbound {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (used, done) = {
            let chunk = match r.fill_buf().await {
                Ok(c) => c,
                Err(_) => return Inbound::Io,
            };
            if chunk.is_empty() {
                // A final line without a newline still counts.
                return if buf.is_empty() { Inbound::Eof } else { Inbound::Frame(buf) };
            }
            match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if buf.len() + i > MAX_FRAME_BYTES {
                        return Inbound::TooLong;
                    }
                    buf.extend_from_slice(&chunk[..i]);
                    (i + 1, true)
                }
                None => {
                    if buf.len() + chunk.len() > MAX_FRAME_BYTES {
                        return Inbound::TooLong;
                    }
                    buf.extend_from_slice(chunk);
                    (chunk.len(), false)
                }
            }
        };
        r.consume(used);
        if done {
            return Inbound::Frame(buf);
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(rd: R, tx: mpsc::Sender<Inbound>) {
    let mut rd = BufReader::new(rd);
    loop {
        let item = read_frame(&mut rd).await;
        let last = !matches!(item, Inbound::Frame(_));
        if tx.send(item).await.is_err() || last {
            return;
        }
    }
}

async fn write_msg<W: AsyncWrite + Unpin>(wr: &mut W, msg: &HostMessage) -> Result<(), ()> {
    let line = encode_line(msg).map_err(|_| ())?;
    wr.write_all(line.as_bytes()).await.map_err(|_| ())?;
    wr.flush().await.map_err(|_| ())
}

fn parse(frame: &[u8]) -> Result<PluginMessage, String> {
    let text = std::str::from_utf8(frame).map_err(|_| "frame is not valid UTF-8".to_owned())?;
    decode_line(text).map_err(|e: FrameError| e.to_string())
}

// -------------------------------------------------------------------------------------------------
// Session
// -------------------------------------------------------------------------------------------------

async fn run_session<T>(stream: T, host: HostHandle, access: Access)
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let (in_tx, mut in_rx) = mpsc::channel(16);
    let reader = tokio::spawn(read_loop(rd, in_tx));
    if let Some((plugin, session)) = session(&mut wr, &mut in_rx, &host, &access).await {
        let _ = host.input.send(Input::Gone { plugin, session }).await;
    }
    reader.abort();
    let _ = wr.shutdown().await;
}

/// Returns the plugin and session id once the handshake succeeded (so the caller can report the end).
async fn session<W: AsyncWrite + Unpin>(
    wr: &mut W,
    in_rx: &mut mpsc::Receiver<Inbound>,
    host: &HostHandle,
    access: &Access,
) -> Option<(String, u64)> {
    let s = &host.settings;
    // Tells the plugin why, then ends the session (a rejected plugin has nothing to report as gone).
    let fail = async |wr: &mut W, message: String| -> Option<(String, u64)> {
        let _ = write_msg(wr, &HostMessage::Error { message }).await;
        None
    };

    // A remote agent is challenged before it says anything: a fresh nonce it must answer in its hello.
    let nonce = if access.remote {
        let nonce = auth::new_nonce().ok()?;
        write_msg(wr, &HostMessage::Challenge { nonce: nonce.clone() }).await.ok()?;
        Some(nonce)
    } else {
        None
    };

    // Handshake.
    let first = match timeout(s.handshake_timeout, in_rx.recv()).await {
        Err(_) => return fail(wr, "handshake timed out".into()).await,
        Ok(None | Some(Inbound::Eof | Inbound::Io)) => return None,
        Ok(Some(Inbound::TooLong)) => return fail(wr, "frame too long".into()).await,
        Ok(Some(Inbound::Frame(f))) => f,
    };
    let (plugin, proto, proof) = match parse(&first) {
        Ok(PluginMessage::Hello { plugin, proto, proof, .. }) => (plugin, proto, proof),
        Ok(_) => return fail(wr, "the first message must be hello".into()).await,
        Err(e) => return fail(wr, e).await,
    };
    if !proto_compatible(proto) {
        return fail(wr, format!("unsupported protocol {proto}; this host speaks {PROTO_VERSION}")).await;
    }
    if access.remote {
        // One generic answer for every way of failing, so a refusal says nothing about which plugin ids
        // exist or whether the token or the id was wrong.
        let right_plugin = access.only_plugin.as_deref() == Some(plugin.as_str()) && s.plugins.contains_key(&plugin);
        let proven = match (&access.token, &nonce) {
            (Some(token), Some(nonce)) => proof.as_deref().is_some_and(|p| auth::verify(token, nonce, &plugin, p)),
            _ => true,
        };
        if !(right_plugin & proven) {
            log::warn!(target: "host", "authentication failed for a remote agent from {} (claimed plugin {plugin:?})", access.peer);
            sleep(s.auth_failure_delay).await;
            return fail(wr, "authentication failed".into()).await;
        }
    } else if s.remote_plugins.contains(&plugin) {
        return fail(
            wr,
            format!("plugin {plugin:?} is a remote agent: it connects to its own TCP listener, not this socket"),
        )
        .await;
    } else if !s.plugins.contains_key(&plugin) {
        return fail(wr, format!("plugin {plugin:?} is not configured")).await;
    }
    let (out_tx, mut out_rx) = mpsc::channel::<HostMessage>(64);
    let (reply_tx, reply_rx) = oneshot::channel();
    let replace = access.remote && access.token.is_some();
    host.input.send(Input::Connect { plugin: plugin.clone(), out: out_tx, replace, reply: reply_tx }).await.ok()?;
    let id = match reply_rx.await {
        Ok(Ok(id)) => id,
        Ok(Err(e)) => return fail(wr, e).await,
        Err(_) => return None,
    };
    let me = Some((plugin.clone(), id));
    if write_msg(wr, &HostMessage::Welcome { proto: PROTO_VERSION }).await.is_err() {
        return me;
    }

    // Established.
    let mut ticker = interval_at(Instant::now() + s.ping_interval, s.ping_interval);
    let mut deadline = Instant::now() + s.idle_timeout;
    loop {
        tokio::select! {
            ev = in_rx.recv() => match ev {
                Some(Inbound::Frame(f)) => {
                    deadline = Instant::now() + s.idle_timeout;
                    match parse(&f) {
                        Ok(PluginMessage::Ping) => {
                            if write_msg(wr, &HostMessage::Pong).await.is_err() { break; }
                        }
                        Ok(PluginMessage::Pong) => {}
                        Ok(PluginMessage::Hello { .. }) => {
                            let _ = fail(wr, "already said hello".into()).await;
                            break;
                        }
                        Ok(msg) => {
                            let m = Input::Message { plugin: plugin.clone(), session: id, msg };
                            if host.input.send(m).await.is_err() { break; }
                        }
                        Err(e) => { let _ = fail(wr, e).await; break; }
                    }
                }
                Some(Inbound::TooLong) => { let _ = fail(wr, "frame too long".into()).await; break; }
                Some(Inbound::Eof | Inbound::Io) | None => break,
            },
            out = out_rx.recv() => match out {
                Some(m) => if write_msg(wr, &m).await.is_err() { break; },
                None => break,
            },
            _ = ticker.tick() => {
                if write_msg(wr, &HostMessage::Ping).await.is_err() { break; }
            }
            _ = sleep_until(deadline) => break,
        }
    }
    me
}

// -------------------------------------------------------------------------------------------------
// Actor
// -------------------------------------------------------------------------------------------------

struct SessionInfo {
    id: u64,
    out: mpsc::Sender<HostMessage>,
}

struct Pending {
    plugin: String,
    session: u64,
    route: Route,
    reply: async_channel::Sender<Result<(), String>>,
    deadline: Instant,
}

struct Actor<S: Store> {
    registry: Registry<S>,
    settings: Arc<HostSettings>,
    frontend: HostEnd,
    sessions: HashMap<String, SessionInfo>,
    pending: BTreeMap<u64, Pending>,
    next_session: u64,
    next_req: u64,
    commands_open: bool,
}

/// Forwards events to the frontend in order, waiting if its channel is full. A closed frontend is
/// not the plugins' problem, so errors are ignored and plugins keep being served.
async fn emit(frontend: &HostEnd, events: Vec<FrontendEvent>) {
    for e in events {
        let _ = frontend.events.send(e).await;
    }
}

impl<S: Store + Send + 'static> Actor<S> {
    async fn run(mut self, mut rx: mpsc::Receiver<Input>) {
        loop {
            let next = self.pending.values().map(|p| p.deadline).min();
            let wake = next.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                input = rx.recv() => match input {
                    Some(i) => self.input(i).await,
                    None => break,
                },
                cmd = self.frontend.commands.recv(), if self.commands_open => match cmd {
                    Ok(c) => self.command(c).await,
                    Err(_) => self.commands_open = false,
                },
                _ = sleep_until(wake), if next.is_some() => self.expire().await,
            }
        }
    }

    fn tell(&self, plugin: &str, msg: HostMessage) -> bool {
        self.sessions.get(plugin).is_some_and(|s| s.out.try_send(msg).is_ok())
    }

    fn current(&self, plugin: &str, session: u64) -> bool {
        self.sessions.get(plugin).is_some_and(|s| s.id == session)
    }

    async fn input(&mut self, input: Input) {
        match input {
            Input::Connect { plugin, out, replace, reply } => {
                if replace && let Some(old) = self.sessions.remove(&plugin) {
                    // The old session is dropped (its connection closes); whatever it was asked to do fails.
                    let stale: Vec<u64> =
                        self.pending.iter().filter(|(_, p)| p.session == old.id).map(|(id, _)| *id).collect();
                    for id in stale {
                        self.finish(id, Err("the plugin reconnected".into())).await;
                    }
                }
                let result = if self.sessions.contains_key(&plugin) {
                    Err(format!("plugin {plugin:?} is already connected"))
                } else {
                    let id = self.next_session;
                    self.next_session += 1;
                    self.sessions.insert(plugin, SessionInfo { id, out });
                    Ok(id)
                };
                let _ = reply.send(result);
            }
            Input::Gone { plugin, session } => {
                if !self.current(&plugin, session) {
                    return;
                }
                self.sessions.remove(&plugin);
                let lost: Vec<u64> =
                    self.pending.iter().filter(|(_, p)| p.session == session).map(|(id, _)| *id).collect();
                for id in lost {
                    self.finish(id, Err("plugin disconnected".into())).await;
                }
                let events = self.registry.plugin_disconnected(&plugin);
                emit(&self.frontend, events).await;
            }
            Input::Message { plugin, session, msg } => {
                if !self.current(&plugin, session) {
                    return;
                }
                self.message(&plugin, msg).await;
            }
        }
    }

    async fn message(&mut self, plugin: &str, msg: PluginMessage) {
        let result = match msg {
            PluginMessage::Register { device } => self.registry.register(plugin, device),
            PluginMessage::Unregister { device } => self.registry.unregister(plugin, &device),
            PluginMessage::Update { device, capability, value } => {
                self.registry.update(plugin, &device, &capability, value)
            }
            PluginMessage::SetResult { req_id, ok, error } => {
                self.set_result(plugin, req_id, ok, error).await;
                return;
            }
            // Handled by the session; Hello cannot get here.
            PluginMessage::Hello { .. } | PluginMessage::Ping | PluginMessage::Pong => return,
        };
        match result {
            Ok(events) => emit(&self.frontend, events).await,
            Err(message) => {
                self.tell(plugin, HostMessage::Error { message });
            }
        }
    }

    async fn set_result(&mut self, plugin: &str, req_id: u64, ok: bool, error: Option<String>) {
        // Only the plugin the request went to may answer it.
        if self.pending.get(&req_id).is_none_or(|p| p.plugin != plugin) {
            return;
        }
        let outcome = if ok { Ok(()) } else { Err(error.unwrap_or_else(|| "the plugin refused the change".into())) };
        self.finish(req_id, outcome).await;
    }

    /// Completes a pending write: on success the registry applies it so controllers see it at once.
    async fn finish(&mut self, req_id: u64, outcome: Result<(), String>) {
        let Some(p) = self.pending.remove(&req_id) else { return };
        if outcome.is_ok() {
            let events = self.registry.confirm(&p.route);
            emit(&self.frontend, events).await;
        }
        let _ = p.reply.send(outcome).await;
    }

    async fn command(&mut self, cmd: ControllerCommand) {
        let route = match self.registry.route(cmd.endpoint, cmd.write) {
            Ok(r) => r,
            Err(e) => {
                let _ = cmd.reply.send(Err(e)).await;
                return;
            }
        };
        let Some(session) = self.sessions.get(&route.plugin).map(|s| s.id) else {
            let _ = cmd.reply.send(Err(format!("plugin {:?} is not connected", route.plugin))).await;
            return;
        };
        let req_id = self.next_req;
        self.next_req += 1;
        let msg = HostMessage::Set {
            req_id,
            device: route.device.clone(),
            capability: route.capability.clone(),
            value: route.value,
        };
        if !self.tell(&route.plugin, msg) {
            let _ = cmd.reply.send(Err(format!("plugin {:?} is busy", route.plugin))).await;
            return;
        }
        let timeout = self.settings.plugins.get(&route.plugin).map_or(Duration::from_secs(2), |p| p.set_timeout);
        self.pending.insert(
            req_id,
            Pending {
                plugin: route.plugin.clone(),
                session,
                route,
                reply: cmd.reply,
                deadline: Instant::now() + timeout,
            },
        );
    }

    async fn expire(&mut self) {
        let now = Instant::now();
        let due: Vec<u64> = self.pending.iter().filter(|(_, p)| p.deadline <= now).map(|(id, _)| *id).collect();
        for id in due {
            self.finish(id, Err("the plugin did not answer in time".into())).await;
        }
    }
}

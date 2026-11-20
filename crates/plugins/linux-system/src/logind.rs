//! The real session source: systemd-logind over the system D-Bus (zbus).

use futures_lite::StreamExt;
use zbus::zvariant::OwnedObjectPath;

use crate::{SessionInfo, SessionSource};

/// `(session id, uid, user name, seat, object path)`.
type SessionRow = (String, u32, String, String, OwnedObjectPath);

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    /// Every session.
    fn list_sessions(&self) -> zbus::Result<Vec<SessionRow>>;

    #[zbus(signal)]
    fn session_new(&self, id: String, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    fn session_removed(&self, id: String, path: OwnedObjectPath) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1")]
trait Session {
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
    #[zbus(property, name = "Type")]
    fn kind(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn remote(&self) -> zbus::Result<bool>;
}

pub struct Logind {
    connection: zbus::Connection,
    manager: ManagerProxy<'static>,
    added: SessionNewStream,
    removed: SessionRemovedStream,
}

impl Logind {
    /// Connects to the system bus and subscribes to session changes.
    pub async fn connect() -> Result<Self, String> {
        let err = |e: zbus::Error| format!("cannot use systemd-logind on the system D-Bus: {e}");
        let connection = zbus::Connection::system().await.map_err(err)?;
        let manager = ManagerProxy::new(&connection).await.map_err(err)?;
        let added = manager.receive_session_new().await.map_err(err)?;
        let removed = manager.receive_session_removed().await.map_err(err)?;
        Ok(Self { connection, manager, added, removed })
    }

    async fn describe(&self, path: OwnedObjectPath) -> zbus::Result<SessionInfo> {
        let s = SessionProxy::builder(&self.connection).path(path)?.build().await?;
        Ok(SessionInfo {
            class: s.class().await?,
            kind: s.kind().await?,
            state: s.state().await?,
            remote: s.remote().await?,
        })
    }
}

impl SessionSource for Logind {
    async fn sessions(&mut self) -> Result<Vec<SessionInfo>, String> {
        let list = self.manager.list_sessions().await.map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for (_id, _uid, _user, _seat, path) in list {
            // A session can end between the listing and the property reads; skip it then.
            if let Ok(info) = self.describe(path).await {
                out.push(info);
            }
        }
        Ok(out)
    }

    async fn changed(&mut self) {
        tokio::select! {
            _ = self.added.next() => {}
            _ = self.removed.next() => {}
        }
    }
}

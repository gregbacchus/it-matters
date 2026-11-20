//! State shared between the Matter handlers: the endpoint table, the node metadata derived from it,
//! and the channels to the host.

use std::sync::{Arc, Mutex, MutexGuard};

use async_channel::{Receiver, Sender};
use it_matters_core::frontend::{ControllerCommand, FrontendEvent, Write};
use rs_matter::dm::{Endpoint, Metadata, Node};
use rs_matter::error::{Error, ErrorCode};

use crate::node;
use crate::state::Table;

pub struct Shared {
    table: Mutex<Table>,
    /// The node's endpoints: root, aggregator, then one per bridged unit. Rebuilt when the table's
    /// topology changes.
    endpoints: Mutex<Vec<Endpoint<'static>>>,
    commands: Sender<ControllerCommand>,
    events: Receiver<FrontendEvent>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock means a handler panicked; the data is still consistent enough to serve.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub fn new(commands: Sender<ControllerCommand>, events: Receiver<FrontendEvent>) -> Arc<Self> {
        let table = Table::default();
        let endpoints = node::endpoints(&table);
        Arc::new(Self { table: Mutex::new(table), endpoints: Mutex::new(endpoints), commands, events })
    }

    pub fn events(&self) -> &Receiver<FrontendEvent> {
        &self.events
    }

    /// Runs `f` on the table.
    pub fn with_table<R>(&self, f: impl FnOnce(&mut Table) -> R) -> R {
        f(&mut lock(&self.table))
    }

    /// Rebuilds the node metadata after the topology changed.
    pub fn rebuild_node(&self) {
        let fresh = node::endpoints(&lock(&self.table));
        *lock(&self.endpoints) = fresh;
    }

    /// Forwards a controller write to the host and waits for the plugin's answer.
    pub async fn write(&self, endpoint: u16, write: Write) -> Result<(), Error> {
        let (reply, answer) = async_channel::bounded(1);
        if self.commands.send(ControllerCommand { endpoint, write, reply }).await.is_err() {
            log::warn!("controller write to endpoint {endpoint} dropped: the host is gone");
            return Err(ErrorCode::Failure.into());
        }
        match answer.recv().await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => {
                log::warn!("controller write to endpoint {endpoint} refused: {reason}");
                Err(ErrorCode::Failure.into())
            }
            Err(_) => Err(ErrorCode::Failure.into()),
        }
    }
}

impl Metadata for Shared {
    fn access<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Node<'_>) -> R,
    {
        let eps = lock(&self.endpoints);
        f(&Node::new(&eps))
    }
}

/// `&Arc<Shared>` as metadata, so the data model can borrow it.
pub struct SharedNode(pub Arc<Shared>);

impl Metadata for SharedNode {
    fn access<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Node<'_>) -> R,
    {
        self.0.access(f)
    }
}

//! The boundary between the host and the Matter side ("frontend").
//!
//! It is a pair of channels, not async trait calls, so the real frontend can run under rs-matter's
//! own executor and drain [`FrontendEvent`]s from inside a handler's `run()`, while the host runs
//! on tokio. `async-channel` is runtime-agnostic.
//!
//! * host -> frontend: [`FrontendEvent`] (topology and state changes), bounded so a flooding plugin
//!   is slowed down instead of growing memory.
//! * frontend -> host: [`ControllerCommand`] (a controller wrote something), each with a reply
//!   channel the host answers once the plugin has applied (or refused) the write.

use async_channel::{Receiver, Sender};

use crate::mapping::UnitKind;

/// Capacity of the host -> frontend channel.
pub const EVENT_CAPACITY: usize = 1024;
/// Capacity of the frontend -> host channel.
pub const COMMAND_CAPACITY: usize = 64;

/// A Matter attribute value, already converted from the plugin's value (levels scaled, mute inverted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrValue {
    /// Boolean State `StateValue` (Contact Sensor).
    BooleanState(bool),
    /// Occupancy Sensing `Occupancy` bit 0.
    Occupancy(bool),
    /// On/Off `OnOff` (a plug, or a speaker where On means *not muted*).
    OnOff(bool),
    /// Level Control `CurrentLevel`, 0..=254.
    CurrentLevel(u8),
}

/// What the frontend needs to create a bridged endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSpec {
    pub endpoint: u16,
    pub kind: UnitKind,
    /// Shown by controllers (Bridged Device Basic Information `NodeLabel`).
    pub name: String,
    /// Stable per unit (Bridged Device Basic Information `UniqueID`): `<device>/<unit>`.
    pub unique_id: String,
    pub reachable: bool,
    /// Initial attribute values (a speaker has a level and an on/off).
    pub attrs: Vec<AttrValue>,
}

/// Host -> frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendEvent {
    AddEndpoint(EndpointSpec),
    RemoveEndpoint { endpoint: u16 },
    SetAttr { endpoint: u16, value: AttrValue },
    SetReachable { endpoint: u16, reachable: bool },
}

/// A write a controller made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    OnOff(bool),
    CurrentLevel(u8),
}

/// Frontend -> host.
#[derive(Debug)]
pub struct ControllerCommand {
    pub endpoint: u16,
    pub write: Write,
    /// The host answers here: `Ok(())` once the plugin applied it, else a reason.
    pub reply: Sender<Result<(), String>>,
}

/// The host's ends of the channels.
#[derive(Debug, Clone)]
pub struct HostEnd {
    pub events: Sender<FrontendEvent>,
    pub commands: Receiver<ControllerCommand>,
}

/// The frontend's ends of the channels.
#[derive(Debug, Clone)]
pub struct FrontendEnd {
    pub events: Receiver<FrontendEvent>,
    pub commands: Sender<ControllerCommand>,
}

/// Creates the two ends.
pub fn channel() -> (HostEnd, FrontendEnd) {
    let (event_tx, event_rx) = async_channel::bounded(EVENT_CAPACITY);
    let (cmd_tx, cmd_rx) = async_channel::bounded(COMMAND_CAPACITY);
    (HostEnd { events: event_tx, commands: cmd_rx }, FrontendEnd { events: event_rx, commands: cmd_tx })
}

/// A stand-in for the Matter side: records every event and lets a test play the controller.
#[derive(Debug)]
pub struct FakeFrontend {
    end: FrontendEnd,
}

impl FakeFrontend {
    pub fn new(end: FrontendEnd) -> Self {
        Self { end }
    }

    /// Every event received since the last call.
    pub fn drain(&self) -> Vec<FrontendEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.end.events.try_recv() {
            out.push(e);
        }
        out
    }

    /// Waits for the next event.
    pub async fn next_event(&self) -> Option<FrontendEvent> {
        self.end.events.recv().await.ok()
    }

    /// Plays a controller write; the returned receiver yields the host's answer.
    pub async fn write(&self, endpoint: u16, write: Write) -> Receiver<Result<(), String>> {
        let (reply, answer) = async_channel::bounded(1);
        self.end
            .commands
            .send(ControllerCommand { endpoint, write, reply })
            .await
            .expect("the host end of the frontend channel is gone");
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_flow_host_to_frontend_in_order() {
        let (host, end) = channel();
        let fake = FakeFrontend::new(end);
        host.events.try_send(FrontendEvent::RemoveEndpoint { endpoint: 2 }).unwrap();
        host.events.try_send(FrontendEvent::SetReachable { endpoint: 3, reachable: false }).unwrap();
        assert_eq!(
            fake.drain(),
            vec![
                FrontendEvent::RemoveEndpoint { endpoint: 2 },
                FrontendEvent::SetReachable { endpoint: 3, reachable: false }
            ]
        );
        assert!(fake.drain().is_empty());
    }

    #[test]
    fn the_event_channel_is_bounded() {
        let (host, _end) = channel();
        for _ in 0..EVENT_CAPACITY {
            host.events.try_send(FrontendEvent::RemoveEndpoint { endpoint: 2 }).unwrap();
        }
        assert!(host.events.try_send(FrontendEvent::RemoveEndpoint { endpoint: 2 }).is_err());
    }

    #[test]
    fn a_controller_write_reaches_the_host_and_the_answer_comes_back() {
        let (host, end) = channel();
        let fake = FakeFrontend::new(end);
        futures_lite_block_on(async {
            let answer = fake.write(4, Write::OnOff(false)).await;
            let cmd = host.commands.recv().await.unwrap();
            assert_eq!((cmd.endpoint, cmd.write), (4, Write::OnOff(false)));
            cmd.reply.send(Err("plugin said no".into())).await.unwrap();
            assert_eq!(answer.recv().await.unwrap(), Err("plugin said no".to_owned()));
        });
    }

    /// A minimal single-future executor, enough for channels that never need a reactor.
    fn futures_lite_block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut f = std::pin::pin!(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }
}

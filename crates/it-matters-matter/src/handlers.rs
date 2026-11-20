//! The cluster handlers. There is **one handler per cluster type**, serving every bridged endpoint:
//! the endpoint id comes from each request and the state lives in the shared table. The topology is
//! therefore dynamic without changing the (static) handler chain.
//!
//! [`BridgedInfoH`] additionally runs the event pump (see [`BridgedInfoH::run`]): it applies the
//! host's events to the table and tells rs-matter which attributes subscribers must be told about.

use std::future::Future;
use std::sync::Arc;

use it_matters_core::frontend::Write;
use rs_matter::dm::clusters::decl::boolean_state::{self, AttributeId as BoolAttr};
use rs_matter::dm::clusters::decl::bridged_device_basic_information::{self as bridged, AttributeId as BridgedAttr};
use rs_matter::dm::clusters::decl::level_control::{self as level, AttributeId as LevelAttr};
use rs_matter::dm::clusters::decl::occupancy_sensing::{self, AttributeId as OccAttr};
use rs_matter::dm::clusters::decl::on_off::{self, AttributeId as OnOffAttr};
use rs_matter::dm::{Cluster, Dataver, HandlerContext, InvokeContext, ReadContext, WriteContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, TLVBuilderParent, Utf8StrBuilder};
use rs_matter::utils::sync::MaybeSend;
use rs_matter::with;

use crate::shared::Shared;
use crate::state::{Changed, EndpointState};

fn fail() -> Error {
    ErrorCode::Failure.into()
}

/// Reads from an endpoint's state; an endpoint that vanished meanwhile is an error.
fn read<R>(shared: &Shared, endpoint: u16, f: impl FnOnce(&EndpointState) -> R) -> Result<R, Error> {
    shared.with_table(|t| t.get(endpoint).map(f)).ok_or_else(fail)
}

macro_rules! plumbing {
    () => {
        fn dataver(&self) -> u32 {
            self.dataver.get()
        }
        fn dataver_changed(&self) {
            self.dataver.changed();
        }
    };
}

// -------------------------------------------------------------------------------------------------

/// Boolean State (Contact Sensor).
pub struct BooleanStateH {
    dataver: Dataver,
    shared: Arc<Shared>,
}

impl BooleanStateH {
    pub fn new(shared: Arc<Shared>, dataver: Dataver) -> Self {
        Self { dataver, shared }
    }
}

impl boolean_state::ClusterAsyncHandler for BooleanStateH {
    const CLUSTER: Cluster<'static> =
        boolean_state::FULL_CLUSTER.with_features(0).with_attrs(with!(required)).with_cmds(with!());
    plumbing!();

    async fn state_value(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        read(&self.shared, ctx.attr().endpoint_id, |s| s.contact)
    }
}

// -------------------------------------------------------------------------------------------------

/// Occupancy Sensing.
pub struct OccupancyH {
    dataver: Dataver,
    shared: Arc<Shared>,
}

impl OccupancyH {
    pub fn new(shared: Arc<Shared>, dataver: Dataver) -> Self {
        Self { dataver, shared }
    }
}

impl occupancy_sensing::ClusterAsyncHandler for OccupancyH {
    const CLUSTER: Cluster<'static> = occupancy_sensing::FULL_CLUSTER
        .with_features(occupancy_sensing::Feature::PHYSICAL_CONTACT.bits())
        .with_attrs(with!(required))
        .with_cmds(with!());
    plumbing!();

    async fn occupancy(&self, ctx: impl ReadContext) -> Result<occupancy_sensing::OccupancyBitmap, Error> {
        let occupied = read(&self.shared, ctx.attr().endpoint_id, |s| s.occupied)?;
        Ok(if occupied {
            occupancy_sensing::OccupancyBitmap::OCCUPIED
        } else {
            occupancy_sensing::OccupancyBitmap::empty()
        })
    }

    async fn occupancy_sensor_type(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<occupancy_sensing::OccupancySensorTypeEnum, Error> {
        Ok(occupancy_sensing::OccupancySensorTypeEnum::PhysicalContact)
    }

    async fn occupancy_sensor_type_bitmap(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<occupancy_sensing::OccupancySensorTypeBitmap, Error> {
        Ok(occupancy_sensing::OccupancySensorTypeBitmap::PHYSICAL_CONTACT)
    }
}

// -------------------------------------------------------------------------------------------------

/// On/Off (a plug, or a speaker's mute where On means *not muted*).
pub struct OnOffH {
    dataver: Dataver,
    shared: Arc<Shared>,
}

impl OnOffH {
    pub fn new(shared: Arc<Shared>, dataver: Dataver) -> Self {
        Self { dataver, shared }
    }

    async fn set(&self, endpoint: u16, on: bool) -> Result<(), Error> {
        self.shared.write(endpoint, Write::OnOff(on)).await
    }
}

impl on_off::ClusterAsyncHandler for OnOffH {
    const CLUSTER: Cluster<'static> = on_off::FULL_CLUSTER
        .with_features(0)
        .with_attrs(with!(required))
        .with_cmds(with!(on_off::CommandId::Off | on_off::CommandId::On | on_off::CommandId::Toggle));
    plumbing!();

    async fn on_off(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        read(&self.shared, ctx.attr().endpoint_id, |s| s.on_off)
    }

    async fn handle_off(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, false).await
    }

    async fn handle_on(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, true).await
    }

    async fn handle_toggle(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        let endpoint = ctx.cmd().endpoint_id;
        let now = read(&self.shared, endpoint, |s| s.on_off)?;
        self.set(endpoint, !now).await
    }

    async fn handle_off_with_effect(
        &self,
        _ctx: impl InvokeContext,
        _request: on_off::OffWithEffectRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_on_with_recall_global_scene(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_on_with_timed_off(
        &self,
        _ctx: impl InvokeContext,
        _request: on_off::OnWithTimedOffRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

// -------------------------------------------------------------------------------------------------

/// Level Control (a speaker's volume). Transitions are not simulated: every command sets the level
/// at once, `Move` jumps to the end of the range and `Stop` does nothing.
pub struct LevelControlH {
    dataver: Dataver,
    shared: Arc<Shared>,
}

impl LevelControlH {
    pub fn new(shared: Arc<Shared>, dataver: Dataver) -> Self {
        Self { dataver, shared }
    }

    async fn set(&self, endpoint: u16, level: u8) -> Result<(), Error> {
        self.shared.write(endpoint, Write::CurrentLevel(level.min(254))).await
    }

    async fn step(&self, endpoint: u16, mode: level::StepModeEnum, size: u8) -> Result<(), Error> {
        let now = read(&self.shared, endpoint, |s| s.level)?;
        let next = match mode {
            level::StepModeEnum::Up => now.saturating_add(size).min(254),
            level::StepModeEnum::Down => now.saturating_sub(size),
        };
        self.set(endpoint, next).await
    }

    async fn go(&self, endpoint: u16, mode: level::MoveModeEnum) -> Result<(), Error> {
        self.set(endpoint, if mode == level::MoveModeEnum::Up { 254 } else { 0 }).await
    }
}

impl level::ClusterAsyncHandler for LevelControlH {
    const CLUSTER: Cluster<'static> = level::FULL_CLUSTER
        .with_features(0)
        .with_attrs(with!(required; LevelAttr::MinLevel | LevelAttr::MaxLevel))
        .with_cmds(with!(
            level::CommandId::MoveToLevel
                | level::CommandId::Move
                | level::CommandId::Step
                | level::CommandId::Stop
                | level::CommandId::MoveToLevelWithOnOff
                | level::CommandId::MoveWithOnOff
                | level::CommandId::StepWithOnOff
                | level::CommandId::StopWithOnOff
        ));
    plumbing!();

    async fn current_level(&self, ctx: impl ReadContext) -> Result<Nullable<u8>, Error> {
        read(&self.shared, ctx.attr().endpoint_id, |s| Nullable::some(s.level))
    }

    async fn min_level(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(0)
    }

    async fn max_level(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(254)
    }

    async fn options(&self, _ctx: impl ReadContext) -> Result<level::OptionsBitmap, Error> {
        Ok(level::OptionsBitmap::empty())
    }

    async fn on_level(&self, _ctx: impl ReadContext) -> Result<Nullable<u8>, Error> {
        Ok(Nullable::none())
    }

    async fn set_options(&self, _ctx: impl WriteContext, _value: level::OptionsBitmap) -> Result<(), Error> {
        Ok(())
    }

    async fn set_on_level(&self, _ctx: impl WriteContext, _value: Nullable<u8>) -> Result<(), Error> {
        Ok(())
    }

    async fn handle_move_to_level(
        &self,
        ctx: impl InvokeContext,
        request: level::MoveToLevelRequest<'_>,
    ) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, request.level()?).await
    }

    async fn handle_move(&self, ctx: impl InvokeContext, request: level::MoveRequest<'_>) -> Result<(), Error> {
        self.go(ctx.cmd().endpoint_id, request.move_mode()?).await
    }

    async fn handle_step(&self, ctx: impl InvokeContext, request: level::StepRequest<'_>) -> Result<(), Error> {
        self.step(ctx.cmd().endpoint_id, request.step_mode()?, request.step_size()?).await
    }

    async fn handle_stop(&self, _ctx: impl InvokeContext, _request: level::StopRequest<'_>) -> Result<(), Error> {
        Ok(())
    }

    async fn handle_move_to_level_with_on_off(
        &self,
        ctx: impl InvokeContext,
        request: level::MoveToLevelWithOnOffRequest<'_>,
    ) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, request.level()?).await
    }

    async fn handle_move_with_on_off(
        &self,
        ctx: impl InvokeContext,
        request: level::MoveWithOnOffRequest<'_>,
    ) -> Result<(), Error> {
        self.go(ctx.cmd().endpoint_id, request.move_mode()?).await
    }

    async fn handle_step_with_on_off(
        &self,
        ctx: impl InvokeContext,
        request: level::StepWithOnOffRequest<'_>,
    ) -> Result<(), Error> {
        self.step(ctx.cmd().endpoint_id, request.step_mode()?, request.step_size()?).await
    }

    async fn handle_stop_with_on_off(
        &self,
        _ctx: impl InvokeContext,
        _request: level::StopWithOnOffRequest<'_>,
    ) -> Result<(), Error> {
        Ok(())
    }

    async fn handle_move_to_closest_frequency(
        &self,
        _ctx: impl InvokeContext,
        _request: level::MoveToClosestFrequencyRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

// -------------------------------------------------------------------------------------------------

/// Bridged Device Basic Information: name, unique id and reachability of each bridged endpoint.
/// Also runs the event pump.
pub struct BridgedInfoH {
    dataver: Dataver,
    shared: Arc<Shared>,
}

impl BridgedInfoH {
    pub fn new(shared: Arc<Shared>, dataver: Dataver) -> Self {
        Self { dataver, shared }
    }
}

/// Tells rs-matter what changed so that subscribers get a report.
fn notify(ctx: &impl HandlerContext, change: Changed) {
    use crate::node::AGGREGATOR_ENDPOINT;
    let attr = |endpoint, cluster: u32, attr: u32| ctx.notify_attr_changed(endpoint, cluster, attr);
    match change {
        Changed::Topology { endpoint } => {
            ctx.notify_endpoint_changed(AGGREGATOR_ENDPOINT);
            ctx.notify_endpoint_changed(endpoint);
        }
        Changed::Reachable { endpoint } => {
            attr(endpoint, <BridgedInfoH as bridged::ClusterAsyncHandler>::CLUSTER.id, BridgedAttr::Reachable as u32)
        }
        Changed::Contact { endpoint } => attr(
            endpoint,
            <BooleanStateH as boolean_state::ClusterAsyncHandler>::CLUSTER.id,
            BoolAttr::StateValue as u32,
        ),
        Changed::Occupancy { endpoint } => attr(
            endpoint,
            <OccupancyH as occupancy_sensing::ClusterAsyncHandler>::CLUSTER.id,
            OccAttr::Occupancy as u32,
        ),
        Changed::OnOff { endpoint } => {
            attr(endpoint, <OnOffH as on_off::ClusterAsyncHandler>::CLUSTER.id, OnOffAttr::OnOff as u32)
        }
        Changed::Level { endpoint } => {
            attr(endpoint, <LevelControlH as level::ClusterAsyncHandler>::CLUSTER.id, LevelAttr::CurrentLevel as u32)
        }
    }
}

impl bridged::ClusterAsyncHandler for BridgedInfoH {
    const CLUSTER: Cluster<'static> = bridged::FULL_CLUSTER
        .with_features(0)
        .with_attrs(with!(required; BridgedAttr::NodeLabel | BridgedAttr::UniqueID))
        .with_cmds(with!());
    plumbing!();

    /// The event pump: applies the host's events to the table, rebuilds the node metadata when the
    /// topology changed, and notifies subscribers. Runs for the life of the Matter stack.
    fn run(&self, ctx: impl HandlerContext) -> impl Future<Output = Result<(), Error>> + MaybeSend {
        let shared = self.shared.clone();
        async move {
            while let Ok(event) = shared.events().recv().await {
                let changes = shared.with_table(|t| t.apply(event));
                if changes.iter().any(|c| matches!(c, Changed::Topology { .. })) {
                    shared.rebuild_node();
                }
                for c in changes {
                    notify(&ctx, c);
                }
            }
            // The host went away: keep serving what we have.
            core::future::pending::<Result<(), Error>>().await
        }
    }

    async fn reachable(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        read(&self.shared, ctx.attr().endpoint_id, |s| s.reachable)
    }

    async fn node_label<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        let name = read(&self.shared, ctx.attr().endpoint_id, |s| s.name.clone())?;
        builder.set(truncate(&name, 32))
    }

    async fn unique_id<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        let id = read(&self.shared, ctx.attr().endpoint_id, |s| s.unique_id.clone())?;
        builder.set(truncate(&id, 32))
    }

    async fn handle_keep_active(
        &self,
        _ctx: impl InvokeContext,
        _request: bridged::KeepActiveRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

/// Matter limits `NodeLabel` and `UniqueID` to 32 characters; cut on a character boundary.
pub fn truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate("short", 32), "short");
        assert_eq!(truncate(&"a".repeat(40), 32).len(), 32);
        assert_eq!(truncate("ééééé", 3), "ééé");
        assert_eq!(truncate("", 5), "");
    }
}

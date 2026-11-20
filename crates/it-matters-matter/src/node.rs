//! The node's endpoint list: the root endpoint, the Aggregator, and one bridged endpoint per unit.

use it_matters_core::mapping::UnitKind;
use rs_matter::dm::Endpoint;
use rs_matter::dm::clusters::decl::boolean_state::ClusterAsyncHandler as _;
use rs_matter::dm::clusters::decl::{
    bridged_device_basic_information::ClusterAsyncHandler as _, level_control::ClusterAsyncHandler as _,
    occupancy_sensing::ClusterAsyncHandler as _, on_off::ClusterAsyncHandler as _,
};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::devices::{
    DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE, DEV_TYPE_CONTACT_SENSOR, DEV_TYPE_OCCUPANCY_SENSOR,
    DEV_TYPE_ON_OFF_PLUG_IN_UNIT, DEV_TYPE_SPEAKER,
};
use rs_matter::{clusters, devices, root_endpoint};

use crate::handlers::{BooleanStateH, BridgedInfoH, LevelControlH, OccupancyH, OnOffH};
use crate::state::Table;

/// The Aggregator endpoint: it lists every bridged endpoint in its parts list.
pub const AGGREGATOR_ENDPOINT: u16 = 1;

const ROOT: Endpoint<'static> = root_endpoint!(eth);
const AGGREGATOR: Endpoint<'static> =
    Endpoint::new(AGGREGATOR_ENDPOINT, devices!(DEV_TYPE_AGGREGATOR), clusters!(desc::DescHandler::CLUSTER));

/// A bridged endpoint of the given kind.
pub fn bridged_endpoint(id: u16, kind: UnitKind) -> Endpoint<'static> {
    match kind {
        UnitKind::ContactSensor => Endpoint::new(
            id,
            devices!(DEV_TYPE_CONTACT_SENSOR, DEV_TYPE_BRIDGED_NODE),
            clusters!(desc::DescHandler::CLUSTER, BridgedInfoH::CLUSTER, BooleanStateH::CLUSTER),
        ),
        UnitKind::OccupancySensor => Endpoint::new(
            id,
            devices!(DEV_TYPE_OCCUPANCY_SENSOR, DEV_TYPE_BRIDGED_NODE),
            clusters!(desc::DescHandler::CLUSTER, BridgedInfoH::CLUSTER, OccupancyH::CLUSTER),
        ),
        UnitKind::OnOffPlugin => Endpoint::new(
            id,
            devices!(DEV_TYPE_ON_OFF_PLUG_IN_UNIT, DEV_TYPE_BRIDGED_NODE),
            clusters!(desc::DescHandler::CLUSTER, BridgedInfoH::CLUSTER, OnOffH::CLUSTER),
        ),
        UnitKind::Speaker => Endpoint::new(
            id,
            devices!(DEV_TYPE_SPEAKER, DEV_TYPE_BRIDGED_NODE),
            clusters!(desc::DescHandler::CLUSTER, BridgedInfoH::CLUSTER, OnOffH::CLUSTER, LevelControlH::CLUSTER),
        ),
    }
}

/// All endpoints of the node, in the increasing id order rs-matter requires.
pub fn endpoints(table: &Table) -> Vec<Endpoint<'static>> {
    let mut v = vec![ROOT, AGGREGATOR];
    v.extend(table.ids().map(|(id, kind)| bridged_endpoint(id, kind)));
    v
}

//! Starts the Matter stack and serves the bridge until it fails.

use core::pin::pin;
use std::path::PathBuf;
use std::sync::Arc;

use it_matters_core::frontend::FrontendEnd;
use rand::RngExt as _;
use rs_matter::crypto::{Crypto, default_crypto};
use rs_matter::dm::clusters::decl::boolean_state::ClusterAsyncHandler as _;
use rs_matter::dm::clusters::decl::bridged_device_basic_information::ClusterAsyncHandler as _;
use rs_matter::dm::clusters::decl::level_control::ClusterAsyncHandler as _;
use rs_matter::dm::clusters::decl::occupancy_sensing::ClusterAsyncHandler as _;
use rs_matter::dm::clusters::decl::on_off::ClusterAsyncHandler as _;
use rs_matter::dm::clusters::decl::{
    boolean_state, bridged_device_basic_information as bridged, level_control, occupancy_sensing, on_off,
};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT};
use rs_matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::{Async, DataModel, Dataver, EmptyHandler};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::pairing::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};
use rs_matter::persist::DirKvBlobStore;
use rs_matter::stack::MatterStack;
use rs_matter::stack::eth::PreexistingEthernet;
use rs_matter::stack::net::std::StdNetStack;
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::{BasicCommData, DevCommData, Matter};
use static_cell::StaticCell;

use crate::commissioning;
use crate::handlers::{BooleanStateH, BridgedInfoH, LevelControlH, OccupancyH, OnOffH};
use crate::node::AGGREGATOR_ENDPOINT;
use crate::shared::{Shared, SharedNode};

/// The stack's bump arena. Unoptimised builds have much larger futures than release builds, and too
/// small an arena panics at start-up ("Out of bump memory"), so this is generous.
const BUMP_SIZE: usize = 64 * 1024;

static MATTER_STACK: StaticCell<MatterStack<BUMP_SIZE, EthNetwork>> = StaticCell::new();

/// Everything the Matter side needs from the configuration.
#[derive(Debug, Clone)]
pub struct Settings {
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub storage_dir: PathBuf,
    pub passcode: Option<u32>,
    pub discriminator: Option<u16>,
}

fn io(msg: String) -> Error {
    log::error!("{msg}");
    ErrorCode::StdIoError.into()
}

/// Runs the Matter stack on the calling thread (rs-matter brings its own executor model, separate
/// from the host's). Returns only if the stack fails. May be called once per process.
pub fn run_blocking(settings: Settings, frontend: FrontendEnd) -> Result<(), Error> {
    let codes = commissioning::resolve(&settings.storage_dir, settings.passcode, settings.discriminator, || {
        rand::rng().random()
    })
    .map_err(io)?;

    // The stack borrows its identity for its whole life; leak it (the process runs one stack).
    let mut details = rs_matter::dm::devices::test::TEST_DEV_DET;
    details.vid = settings.vendor_id;
    details.pid = settings.product_id;
    details.device_name = Box::leak(settings.name.clone().into_boxed_str());
    details.product_name = "it-matters bridge";
    details.vendor_name = "it-matters";
    let details: &'static _ = Box::leak(Box::new(details));
    let password =
        Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(&codes.passcode.to_le_bytes()));
    let comm = DevCommData::Passcode(BasicCommData { password, discriminator: codes.discriminator });

    let stack = MATTER_STACK.uninit().init_with(MatterStack::init(details, comm, &TEST_DEV_ATT));

    std::fs::create_dir_all(&settings.storage_dir)
        .map_err(|e| io(format!("{}: {e}", settings.storage_dir.display())))?;
    let mut store = DirKvBlobStore::new(settings.storage_dir.join("matter"));
    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);
    let mut rng = crypto.rand()?;

    // Re-hydrates persisted state; if not commissioned yet it prints the QR and pairing codes.
    futures_lite::future::block_on(stack.startup(&crypto, &mut store))?;
    let kv = stack.kv(store);

    let shared = Shared::new(frontend.commands.clone(), frontend.events.clone());
    let mdns = rs_matter::transport::network::mdns::avahi::AvahiMdns::new(
        futures_lite::future::block_on(rs_matter::utils::zbus::Connection::system())
            .map_err(|e| io(format!("cannot reach the system D-Bus (is avahi-daemon running?): {e}")))?,
    );

    let matter = pin!(stack.run_eth(
        PreexistingEthernet::new(StdNetStack::new(), SysNetifs, &(), mdns),
        &crypto,
        data_model(stack.matter(), &mut rng, &shared),
        kv,
        (),
    ));
    futures_lite::future::block_on(matter)
}

/// The data model: dynamic node metadata plus one handler per cluster type.
fn data_model<'a>(
    matter: &'a Matter<'a>,
    rng: &mut (impl rs_matter::crypto::Rng + Copy),
    shared: &Arc<Shared>,
) -> impl DataModel + 'a {
    let dataver = |rng: &mut _| Dataver::new_rand(rng);
    (
        SharedNode(shared.clone()),
        EmptyHandler
            .chain(|e, _| e == ROOT_ENDPOINT_ID, Async(MatterStack::<0, EthNetwork>::root_handler(matter, &(), *rng)))
            .chain(
                |e, c| e == AGGREGATOR_ENDPOINT && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new_aggregator(dataver(rng)).adapt()),
            )
            .chain(
                |e, c| e > AGGREGATOR_ENDPOINT && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new(dataver(rng)).adapt()),
            )
            .chain(
                |e, c| e > AGGREGATOR_ENDPOINT && c == BridgedInfoH::CLUSTER.id,
                bridged::HandlerAsyncAdaptor(BridgedInfoH::new(shared.clone(), dataver(rng))),
            )
            .chain(
                |e, c| e > AGGREGATOR_ENDPOINT && c == BooleanStateH::CLUSTER.id,
                boolean_state::HandlerAsyncAdaptor(BooleanStateH::new(shared.clone(), dataver(rng))),
            )
            .chain(
                |e, c| e > AGGREGATOR_ENDPOINT && c == OccupancyH::CLUSTER.id,
                occupancy_sensing::HandlerAsyncAdaptor(OccupancyH::new(shared.clone(), dataver(rng))),
            )
            .chain(
                |e, c| e > AGGREGATOR_ENDPOINT && c == OnOffH::CLUSTER.id,
                on_off::HandlerAsyncAdaptor(OnOffH::new(shared.clone(), dataver(rng))),
            )
            .chain(
                |e, c| e > AGGREGATOR_ENDPOINT && c == LevelControlH::CLUSTER.id,
                level_control::HandlerAsyncAdaptor(LevelControlH::new(shared.clone(), dataver(rng))),
            ),
    )
}

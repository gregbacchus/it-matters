//! `it-matters run`: starts the Matter stack and the plugin host and serves until stopped.
//!
//! Threads: rs-matter runs on its own thread with its own executor (it blocks); the plugin host runs
//! on a small tokio runtime on the main thread. They talk through the channels in
//! `it_matters_core::frontend`.

use std::collections::BTreeMap;
use std::time::Duration;

use it_matters_core::allocator::{Allocator, FileStore};
use it_matters_core::frontend;
use it_matters_core::host::{HostSettings, PluginSettings, bind_unix, serve_unix, spawn_host};
use it_matters_core::registry::Registry;
use it_matters_matter::Settings;

use crate::config::{Config, DEFAULT_PLUGIN_TIMEOUT_MS};

/// Per-plugin host settings from the configuration.
pub fn host_settings(cfg: &Config) -> HostSettings {
    let plugins: BTreeMap<String, PluginSettings> = cfg
        .plugins
        .iter()
        .map(|p| {
            let ms = p.run.as_ref().map_or(DEFAULT_PLUGIN_TIMEOUT_MS, |r| r.timeout_ms);
            (p.id.clone(), PluginSettings { set_timeout: Duration::from_millis(ms) })
        })
        .collect();
    HostSettings::new(plugins)
}

/// The Matter stack's settings from the configuration.
pub fn matter_settings(cfg: &Config) -> Settings {
    Settings {
        name: cfg.bridge.name.clone(),
        vendor_id: cfg.bridge.vendor_id,
        product_id: cfg.bridge.product_id,
        storage_dir: cfg.matter.storage_dir.clone(),
        passcode: cfg.matter.passcode,
        discriminator: cfg.matter.discriminator,
    }
}

/// Runs until a termination signal arrives (`Ok`) or something fails (`Err`).
pub fn run(cfg: Config) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(serve(cfg))
}

async fn serve(cfg: Config) -> Result<(), String> {
    let (host_end, frontend_end) = frontend::channel();

    // Matter first: it prints the pairing codes and fails fast if D-Bus/avahi are unavailable.
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
    let settings = matter_settings(&cfg);
    std::thread::Builder::new()
        .name("matter".into())
        .spawn(move || {
            let result = it_matters_matter::run_blocking(settings, frontend_end);
            let _ = done_tx.send(result.map_err(|e| format!("the Matter stack stopped: {e:?}")));
        })
        .map_err(|e| e.to_string())?;

    let ids = Allocator::open(FileStore::new(cfg.matter.storage_dir.join("endpoints.json")))?;
    let overrides = cfg.devices.iter().filter_map(|(id, d)| d.name.clone().map(|n| (id.clone(), n))).collect();
    let registry = Registry::new(ids, overrides);
    let (host, _actor) = spawn_host(host_settings(&cfg), registry, host_end);

    let listener = bind_unix(&cfg.ipc.socket).map_err(|e| format!("{}: {e}", cfg.ipc.socket.display()))?;
    log::info!("plugins connect to {}", cfg.ipc.socket.display());
    tokio::spawn(serve_unix(listener, host));

    for p in &cfg.plugins {
        if p.connect.is_some() {
            log::warn!("plugin {:?}: `connect` (remote TCP agents) is not implemented yet; it cannot connect", p.id);
        } else {
            log::info!("plugin {:?}: waiting for it to connect (the daemon does not start plugins yet)", p.id);
        }
    }

    let result = tokio::select! {
        r = &mut done_rx => Err(r.unwrap_or_else(|_| Err("the Matter thread ended unexpectedly".into())).err().unwrap_or_else(|| "the Matter stack stopped".into())),
        () = shutdown_signal() => {
            log::info!("shutting down");
            Ok(())
        }
    };
    // Leave no stale socket behind (a stale one is also replaced on the next start).
    let _ = std::fs::remove_file(&cfg.ipc.socket);
    result
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        return std::future::pending().await;
    };
    tokio::select! { _ = term.recv() => {}, _ = int.recv() => {} }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_timeouts_come_from_run_plugins_and_default_otherwise() {
        let cfg = Config::from_yaml(
            "plugins:\n  - { id: a, run: { command: x, timeout_ms: 500 } }\n  - { id: b, connect: { listen: '127.0.0.1:1' } }\n",
        )
        .unwrap();
        let s = host_settings(&cfg);
        assert_eq!(s.plugins["a"].set_timeout, Duration::from_millis(500));
        assert_eq!(s.plugins["b"].set_timeout, Duration::from_millis(DEFAULT_PLUGIN_TIMEOUT_MS));
        assert_eq!(s.plugins.len(), 2);
    }

    #[test]
    fn matter_settings_carry_the_configuration() {
        let cfg = Config::from_yaml(
            "bridge: { name: B, vendor_id: 1, product_id: 2 }\nmatter: { storage_dir: /x, passcode: 20202021, discriminator: 7 }\n",
        )
        .unwrap();
        let m = matter_settings(&cfg);
        assert_eq!((m.name.as_str(), m.vendor_id, m.product_id), ("B", 1, 2));
        assert_eq!((m.passcode, m.discriminator), (Some(20202021), Some(7)));
        assert_eq!(m.storage_dir, std::path::PathBuf::from("/x"));
    }
}

//! Everything we know about iwd: typed D-Bus proxies, the data types the
//! rest of the app consumes, and a high-level [`IwdManager`] wrapper.
//!
//! Design rules:
//! * Station object paths are dynamic — resolve via ObjectManager, never
//!   hardcode `/net/connman/iwd/0`.
//! * Fetches are few and wide: `get_networks` uses exactly 2 D-Bus calls
//!   (GetOrderedNetworks + one GetManagedObjects cross-reference).
//! * Failures propagate as `AppResult` errors; the app surfaces them in the
//!   status bar so they are never silent.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{mpsc, Mutex};
use zbus::fdo::{ObjectManagerProxy, PropertiesProxy};
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::Connection;

use crate::{err, AppResult};

pub const IWD_SERVICE: &str = "net.connman.iwd";
pub const IWD_ROOT: &str = "/";
const IF_STATION: &str = "net.connman.iwd.Station";
const IF_NETWORK: &str = "net.connman.iwd.Network";
const IF_KNOWN_NETWORK: &str = "net.connman.iwd.KnownNetwork";

// ── data types ───────────────────────────────────────────────────

/// A network visible in the scan results of the active Station.
#[derive(Debug, Clone)]
pub struct AppNetwork {
    pub path: OwnedObjectPath,
    pub name: String,
    pub connected: bool,
    /// RSSI in dBm. iwd reports centi-dBm over D-Bus; we convert exactly
    /// once, here, so every consumer can use plain dBm values.
    pub signal_dbm: i16,
    /// iwd security type: "open", "psk", "8021x", "wep", ...
    pub security_type: String,
}

/// A saved (known) network.
#[derive(Debug, Clone)]
pub struct AppKnownNetwork {
    pub path: OwnedObjectPath,
    pub name: String,
    pub security_type: String,
    pub auto_connect: bool,
    pub hidden: bool,
    /// iwd returns a free-form human string (absent if never connected).
    pub last_connected: Option<String>,
}

/// Coalesced change notifications from iwd D-Bus signals.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
pub enum IwdEvent {
    NetworksChanged,
    KnownNetworksChanged,
    ConnectedNetworkChanged,
}

// ── typed proxy declarations ─────────────────────────────────────
// Method names are snake_case and mapped to PascalCase D-Bus members by
// the `#[proxy]` macro. Property setters must be named `set_*`.

#[proxy(
    interface = "net.connman.iwd.Adapter",
    default_service = "net.connman.iwd"
)]
pub trait Adapter {
    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String>;

    #[zbus(property)]
    fn powered(&self) -> zbus::fdo::Result<bool>;

    #[zbus(property)]
    fn set_powered(&self, powered: bool) -> zbus::Result<()>;
}

#[proxy(
    interface = "net.connman.iwd.Station",
    default_service = "net.connman.iwd"
)]
pub trait Station {
    fn scan(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;

    /// Returns `a(on)`: object path + signal strength in **centi-dBm**
    /// (e.g. -5200 means -52 dBm). Never treat the `n` as an SSID string.
    fn get_ordered_networks(&self) -> zbus::Result<Vec<(OwnedObjectPath, i16)>>;

    fn connect_hidden_network(&self, name: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn connected_network(&self) -> zbus::fdo::Result<OwnedObjectPath>;

    #[zbus(property)]
    fn state(&self) -> zbus::fdo::Result<String>;
}

#[proxy(
    interface = "net.connman.iwd.Network",
    default_service = "net.connman.iwd"
)]
pub trait Network {
    fn connect(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String>;
}

#[proxy(
    interface = "net.connman.iwd.KnownNetwork",
    default_service = "net.connman.iwd"
)]
pub trait KnownNetwork {
    fn forget(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String>;

    #[zbus(property)]
    fn auto_connect(&self) -> zbus::fdo::Result<bool>;

    #[zbus(property)]
    fn set_auto_connect(&self, auto_connect: bool) -> zbus::Result<()>;
}

#[proxy(
    interface = "net.connman.iwd.AgentManager",
    default_service = "net.connman.iwd",
    default_path = "/net/connman/iwd"
)]
pub trait AgentManager {
    fn register_agent(&self, path: &OwnedObjectPath) -> zbus::Result<()>;
    fn unregister_agent(&self, path: &OwnedObjectPath) -> zbus::Result<()>;
}

// ── property helpers ─────────────────────────────────────────────

/// Extract a `String` property from a D-Bus `{sv}` dictionary.
///
/// `OwnedValue` needs `try_clone()` (fallible deep copy) before
/// `TryInto<String>` — this is the pattern proven to compile on zbus 4.
fn prop_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props.get(key)?.try_clone().ok()?.try_into().ok()
}

/// Extract a `bool` property from a D-Bus `{sv}` dictionary.
fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props.get(key)?.try_clone().ok()?.try_into().ok()
}

// ── the manager ──────────────────────────────────────────────────

#[derive(Clone)]
pub struct IwdManager {
    pub conn: Option<Connection>,
    station_path: Arc<Mutex<Option<OwnedObjectPath>>>,
}

impl IwdManager {
    pub fn new(conn: Option<Connection>) -> Self {
        Self {
            conn,
            station_path: Arc::new(Mutex::new(None)),
        }
    }

    fn conn(&self) -> AppResult<&Connection> {
        self.conn
            .as_ref()
            .ok_or_else(|| err("The connection to iwd was closed"))
    }

    // --------------------------------------------------------------
    // Station path resolution
    // --------------------------------------------------------------

    /// Resolve and cache the Station path once at startup.
    pub async fn init_station_path(&self) -> AppResult<()> {
        {
            let path = self.station_path.lock().await;
            if path.is_some() {
                return Ok(());
            }
        }
        self.get_station_path().await.map(|_| ())
    }

    /// Resolve the Station path, querying ObjectManager when not cached.
    /// This guarantees recovery if iwd (or the Station) appears late — e.g.
    /// after the Wi-Fi radio is powered on.
    pub async fn get_station_path(&self) -> AppResult<OwnedObjectPath> {
        {
            let path = self.station_path.lock().await;
            if let Some(p) = path.as_ref() {
                return Ok(p.clone());
            }
        }
        let conn = self.conn()?;
        let om = ObjectManagerProxy::new(conn, IWD_SERVICE, IWD_ROOT).await?;
        let objects = om.get_managed_objects().await?;
        for (p, interfaces) in objects {
            if interfaces.contains_key(IF_STATION) {
                let mut path = self.station_path.lock().await;
                *path = Some(p.clone());
                return Ok(p);
            }
        }
        Err(err("No Wi-Fi device was found — is the radio switched on?"))
    }

    /// Drop the cached Station path (station removed / radio powered off /
    /// adapter re-appeared). The next call re-resolves dynamically.
    pub async fn invalidate_station_path(&self) {
        let mut path = self.station_path.lock().await;
        *path = None;
    }

    /// The adapter lives at the *parent* path of the Station
    /// (`/net/connman/iwd/<phy>` vs `/net/connman/iwd/<phy>/<dev>`).
    pub async fn get_adapter_path(&self) -> AppResult<OwnedObjectPath> {
        let station = self.get_station_path().await?;
        let s = station.as_str();
        let parent = match s.rsplit_once('/') {
            Some((p, _)) if !p.is_empty() => p.to_string(),
            _ => s.to_string(),
        };
        OwnedObjectPath::try_from(parent)
            .map_err(|e| err(format!("Invalid adapter path derived from {s}: {e}")))
    }

    // --------------------------------------------------------------
    // Device / radio state
    // --------------------------------------------------------------

    pub async fn get_device_name(&self) -> Option<String> {
        let conn = self.conn().ok()?.clone();
        let adapter_path = self.get_adapter_path().await.ok()?;
        let adapter = AdapterProxy::new(&conn, adapter_path).await.ok()?;
        adapter.name().await.ok()
    }

    pub async fn is_wifi_powered(&self) -> AppResult<bool> {
        let conn = self.conn()?.clone();
        let adapter_path = self.get_adapter_path().await?;
        let adapter = AdapterProxy::new(&conn, adapter_path).await?;
        Ok(adapter.powered().await?)
    }

    pub async fn set_wifi_powered(&self, on: bool) -> AppResult<()> {
        let conn = self.conn()?.clone();
        let adapter_path = self.get_adapter_path().await?;
        let adapter = AdapterProxy::new(&conn, adapter_path).await?;
        adapter.set_powered(on).await?;
        Ok(())
    }

    pub async fn get_station_state(&self) -> AppResult<String> {
        let conn = self.conn()?.clone();
        let station = StationProxy::new(&conn, self.get_station_path().await?).await?;
        Ok(station.state().await?)
    }

    // --------------------------------------------------------------
    // Networks
    // --------------------------------------------------------------

    /// Exactly 2 D-Bus calls: GetOrderedNetworks + GetManagedObjects.
    /// Signal strength arrives in centi-dBm and is converted to dBm here.
    pub async fn get_networks(&self) -> AppResult<Vec<AppNetwork>> {
        let conn = self.conn()?.clone();
        let station_path = self.get_station_path().await?;
        let station = StationProxy::new(&conn, station_path).await?;
        let ordered = station.get_ordered_networks().await?;

        let om = ObjectManagerProxy::new(&conn, IWD_SERVICE, IWD_ROOT).await?;
        let objects = om.get_managed_objects().await?;

        let mut networks = Vec::with_capacity(ordered.len());
        let empty: HashMap<String, OwnedValue> = HashMap::new();
        for (path, centi_dbm) in ordered {
            let props = objects
                .get(&path)
                .and_then(|ifs| ifs.get(IF_NETWORK))
                .unwrap_or(&empty);

            let name = prop_str(props, "Name").unwrap_or_else(|| "Unknown".to_string());
            let security_type = prop_str(props, "Type").unwrap_or_else(|| "?".to_string());
            let connected = prop_bool(props, "Connected").unwrap_or(false);

            networks.push(AppNetwork {
                path,
                name,
                connected,
                signal_dbm: (f32::from(centi_dbm) / 100.0).round() as i16,
                security_type,
            });
        }
        Ok(networks)
    }

    /// Single GetManagedObjects call. `LastConnectedTime` is a *string*
    /// property in iwd, not a number.
    pub async fn get_known_networks(&self) -> AppResult<Vec<AppKnownNetwork>> {
        let conn = self.conn()?.clone();
        let manager = ObjectManagerProxy::new(&conn, IWD_SERVICE, IWD_ROOT).await?;
        let objects = manager.get_managed_objects().await?;
        let mut networks = Vec::new();
        for (path, interfaces) in objects {
            if let Some(props) = interfaces.get(IF_KNOWN_NETWORK) {
                let name = prop_str(props, "Name").unwrap_or_default();
                let security_type = prop_str(props, "Type").unwrap_or_else(|| "?".to_string());
                let auto_connect = prop_bool(props, "AutoConnect").unwrap_or(true);
                let hidden = prop_bool(props, "Hidden").unwrap_or(false);
                let last_connected = prop_str(props, "LastConnectedTime")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                networks.push(AppKnownNetwork {
                    path,
                    name,
                    security_type,
                    auto_connect,
                    hidden,
                    last_connected,
                });
            }
        }
        networks.sort_by_key(|n| n.name.to_lowercase());
        Ok(networks)
    }

    // --------------------------------------------------------------
    // Actions
    // --------------------------------------------------------------

    pub async fn connect_network(&self, path: OwnedObjectPath) -> AppResult<()> {
        let conn = self.conn()?.clone();
        let network = NetworkProxy::new(&conn, path).await?;
        network.connect().await?;
        Ok(())
    }

    pub async fn connect_hidden_network(&self, name: &str) -> AppResult<()> {
        let conn = self.conn()?.clone();
        let station = StationProxy::new(&conn, self.get_station_path().await?).await?;
        station.connect_hidden_network(name).await?;
        Ok(())
    }

    pub async fn disconnect(&self) -> AppResult<()> {
        let conn = self.conn()?.clone();
        let station = StationProxy::new(&conn, self.get_station_path().await?).await?;
        station.disconnect().await?;
        Ok(())
    }

    pub async fn trigger_scan(&self) -> AppResult<()> {
        let conn = self.conn()?.clone();
        let station = StationProxy::new(&conn, self.get_station_path().await?).await?;
        station.scan().await?;
        Ok(())
    }

    pub async fn forget_known_network(&self, path: &OwnedObjectPath) -> AppResult<()> {
        let conn = self.conn()?.clone();
        let proxy = KnownNetworkProxy::new(&conn, path.clone()).await?;
        proxy.forget().await?;
        Ok(())
    }

    pub async fn set_auto_connect(&self, path: &OwnedObjectPath, enabled: bool) -> AppResult<()> {
        let conn = self.conn()?.clone();
        let proxy = KnownNetworkProxy::new(&conn, path.clone()).await?;
        proxy.set_auto_connect(enabled).await?;
        Ok(())
    }

    // --------------------------------------------------------------
    // Signal listeners
    // --------------------------------------------------------------

    /// Spawn background listeners for iwd signals. Events are *filtered* by
    /// interface/property instead of firing on every PropertiesChanged blip,
    /// and Station interfaces appearing/disappearing invalidate the cached
    /// station path so the app recovers from radio power toggles and adapter
    /// re-insertion.
    pub fn spawn_signal_listener(&self) -> mpsc::Receiver<IwdEvent> {
        let (tx, rx) = mpsc::channel(64);
        let Some(conn) = self.conn.clone() else {
            return rx; // no connection, no events
        };
        let manager = self.clone();

        tokio::spawn(async move {
            // -- 1) Station property changes (State / ConnectedNetwork / Networks)
            {
                let tx = tx.clone();
                let conn = conn.clone();
                let manager = manager.clone();
                tokio::spawn(async move {
                    loop {
                        let Ok(path) = manager.get_station_path().await else {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        };
                        let Ok(props) = PropertiesProxy::new(&conn, IWD_SERVICE, path).await else {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        };
                        let Ok(mut stream) = props.receive_properties_changed().await else {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        };
                        while let Some(sig) = stream.next().await {
                            let Ok(args) = sig.args() else { continue };
                            if args.interface_name.as_str() != IF_STATION {
                                continue;
                            }
                            for key in args.changed_properties.keys() {
                                match *key {
                                    "Networks" => {
                                        let _ = tx.try_send(IwdEvent::NetworksChanged);
                                    }
                                    "State" | "ConnectedNetwork" => {
                                        let _ = tx.try_send(IwdEvent::ConnectedNetworkChanged);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                });
            }

            // -- 2) ObjectManager: InterfacesAdded
            {
                let tx = tx.clone();
                let conn = conn.clone();
                let manager = manager.clone();
                tokio::spawn(async move {
                    loop {
                        let Ok(om) = ObjectManagerProxy::new(&conn, IWD_SERVICE, IWD_ROOT).await
                        else {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        };
                        let Ok(mut stream) = om.receive_interfaces_added().await else {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        };
                        while let Some(sig) = stream.next().await {
                            let Ok(args) = sig.args() else { continue };
                            let mut nets = false;
                            let mut known = false;
                            for name in args.interfaces_and_properties.keys() {
                                match *name {
                                    IF_STATION => {
                                        // A (new) Station appeared — the cached
                                        // path may be stale or was unresolvable.
                                        manager.invalidate_station_path().await;
                                    }
                                    IF_NETWORK => nets = true,
                                    IF_KNOWN_NETWORK => known = true,
                                    _ => {}
                                }
                            }
                            if nets {
                                let _ = tx.try_send(IwdEvent::NetworksChanged);
                            }
                            if known {
                                let _ = tx.try_send(IwdEvent::KnownNetworksChanged);
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                });
            }

            // -- 3) ObjectManager: InterfacesRemoved
            {
                let tx = tx.clone();
                let conn = conn.clone();
                let manager = manager.clone();
                tokio::spawn(async move {
                    loop {
                        let Ok(om) = ObjectManagerProxy::new(&conn, IWD_SERVICE, IWD_ROOT).await
                        else {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        };
                        let Ok(mut stream) = om.receive_interfaces_removed().await else {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        };
                        while let Some(sig) = stream.next().await {
                            let Ok(args) = sig.args() else { continue };
                            let mut nets = false;
                            let mut known = false;
                            for name in args.interfaces.iter() {
                                match *name {
                                    IF_STATION => {
                                        manager.invalidate_station_path().await;
                                    }
                                    IF_NETWORK => nets = true,
                                    IF_KNOWN_NETWORK => known = true,
                                    _ => {}
                                }
                            }
                            if nets {
                                let _ = tx.try_send(IwdEvent::NetworksChanged);
                            }
                            if known {
                                let _ = tx.try_send(IwdEvent::KnownNetworksChanged);
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                });
            }
        });

        rx
    }
}

use std::sync::Arc;
use futures_util::StreamExt;
use tokio::sync::Mutex;
use zbus::fdo::{ObjectManagerProxy, PropertiesProxy};
use zbus::zvariant::OwnedObjectPath;
use zbus::{proxy, Connection};
use crate::{AppResult, err};

#[proxy(
    interface = "net.connman.iwd.Adapter",
    default_service = "net.connman.iwd"
)]
trait Adapter {
    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String>;
}

#[proxy(
    interface = "net.connman.iwd.Station",
    default_service = "net.connman.iwd"
)]
trait Station {
    fn scan(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
    fn get_ordered_networks(&self) -> zbus::Result<Vec<(OwnedObjectPath, String)>>;
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
trait Network {
    fn connect(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String>;

    #[zbus(property)]
    fn signal(&self) -> zbus::fdo::Result<i16>;

    #[zbus(property)]
    fn type_(&self) -> zbus::fdo::Result<String>;

    #[zbus(property)]
    fn connected(&self) -> zbus::fdo::Result<bool>;

    #[zbus(property)]
    fn known_network(&self) -> zbus::fdo::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "net.connman.iwd.KnownNetwork",
    default_service = "net.connman.iwd"
)]
trait KnownNetwork {
    fn forget(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String>;

    #[zbus(property)]
    fn type_(&self) -> zbus::fdo::Result<String>;

    #[zbus(property)]
    fn hidden(&self) -> zbus::fdo::Result<bool>;

    #[zbus(property)]
    fn auto_connect(&self) -> zbus::fdo::Result<bool>;

    #[zbus(property)]
    fn set_auto_connect(&self, value: bool) -> zbus::fdo::Result<()>;

    #[zbus(property)]
    fn last_connected_time(&self) -> zbus::fdo::Result<u64>;
}

#[proxy(
    interface = "net.connman.iwd.AgentManager",
    default_service = "net.connman.iwd",
    default_path = "/net/connman/iwd"
)]
trait AgentManager {
    fn register_agent(&self, path: &OwnedObjectPath) -> zbus::Result<()>;
    fn unregister_agent(&self, path: &OwnedObjectPath) -> zbus::Result<()>;
}

#[derive(Debug, Clone)]
pub struct AppNetwork {
    pub path: OwnedObjectPath,
    pub name: String,
    pub connected: bool,
    pub signal_strength: i16,
    pub security_type: String,
    #[allow(dead_code)]
    pub known_path: Option<OwnedObjectPath>,
}

#[derive(Debug, Clone)]
pub struct AppKnownNetwork {
    pub path: OwnedObjectPath,
    pub name: String,
    pub security_type: String,
    pub auto_connect: bool,
    pub hidden: bool,
    pub last_connected: Option<u64>,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
pub enum IwdEvent {
    NetworksChanged,
    KnownNetworksChanged,
    ConnectedNetworkChanged,
}

#[derive(Clone)]
pub struct IwdManager {
    pub conn: Option<Connection>,
    station_path: Arc<Mutex<Option<OwnedObjectPath>>>,
}

impl IwdManager {
    pub fn new(conn: Option<Connection>) -> Self {
        Self { conn, station_path: Arc::new(Mutex::new(None)) }
    }

    fn conn(&self) -> AppResult<&Connection> {
        self.conn.as_ref().ok_or_else(|| err("D-Bus connection not available"))
    }

    pub async fn init_station_path(&self) -> AppResult<()> {
        let mut path = self.station_path.lock().await;
        if path.is_some() {
            return Ok(());
        }
        let conn = self.conn()?;
        let om = ObjectManagerProxy::new(conn, "net.connman.iwd", "/").await?;
        let objects = om.get_managed_objects().await?;
        for (p, interfaces) in objects {
            if interfaces.contains_key("net.connman.iwd.Station") {
                *path = Some(p);
                return Ok(());
            }
        }
        Err(err("No IWD Station found. Is Wi-Fi powered on?"))
    }

    async fn get_station_path(&self) -> AppResult<OwnedObjectPath> {
        let mut path = self.station_path.lock().await;
        if let Some(p) = path.as_ref() {
            return Ok(p.clone());
        }
        // Dynamically query if not cached
        let conn = self.conn()?;
        let om = ObjectManagerProxy::new(conn, "net.connman.iwd", "/").await?;
        let objects = om.get_managed_objects().await?;
        for (p, interfaces) in objects {
            if interfaces.contains_key("net.connman.iwd.Station") {
                *path = Some(p.clone());
                return Ok(p);
            }
        }
        Err(err("No IWD Station found. Is Wi-Fi powered on?"))
    }

    pub async fn get_device_name(&self) -> Option<String> {
        let station_path = self.get_station_path().await.ok()?;
        let path_str = station_path.as_str();
        if let Some(idx) = path_str.rfind('/') {
            let adapter_path = path_str[..idx].to_string();
            if let Ok(conn) = self.conn() {
                if let Ok(adapter) = AdapterProxy::new(conn, adapter_path).await {
                    return adapter.name().await.ok();
                }
            }
        }
        None
    }

    pub async fn get_networks(&self) -> AppResult<Vec<AppNetwork>> {
        let conn = self.conn()?.clone();
        let station_path = self.get_station_path().await?;
        let station = StationProxy::new(&conn, station_path).await?;
        let ordered = station.get_ordered_networks().await?;
        let connected_path = station.connected_network().await.ok();

        let mut networks = Vec::with_capacity(ordered.len());
        for (path, name) in ordered {
            let net_proxy = match NetworkProxy::new(&conn, path.clone()).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            let signal = net_proxy.signal().await.unwrap_or(-100);
            let connected = connected_path.as_ref() == Some(&path)
                || net_proxy.connected().await.unwrap_or(false);
            let security_type = net_proxy.type_().await.unwrap_or_else(|_| "?".to_string());
            let known_path = net_proxy.known_network().await.ok();
            networks.push(AppNetwork {
                path,
                name,
                connected,
                signal_strength: signal,
                security_type,
                known_path,
            });
        }
        Ok(networks)
    }

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

    pub async fn get_known_networks(&self) -> AppResult<Vec<AppKnownNetwork>> {
        let conn = self.conn()?.clone();
        let manager = ObjectManagerProxy::new(&conn, "net.connman.iwd", "/").await?;
        let objects = manager.get_managed_objects().await?;
        let mut networks = Vec::new();
        for (path, interfaces) in objects {
            if let Some(props) = interfaces.get("net.connman.iwd.KnownNetwork") {
                let name = props
                    .get("Name")
                    .and_then(|v| v.try_into().ok())
                    .unwrap_or_default();
                let security_type = props
                    .get("Type")
                    .and_then(|v| v.try_into().ok())
                    .unwrap_or_else(|| "?".to_string());
                let auto_connect = props
                    .get("AutoConnect")
                    .and_then(|v| v.try_into().ok())
                    .unwrap_or(true);
                let hidden = props
                    .get("Hidden")
                    .and_then(|v| v.try_into().ok())
                    .unwrap_or(false);
                let last_connected = props
                    .get("LastConnectedTime")
                    .and_then(|v| v.try_into().ok())
                    .filter(|&t| t > 0);
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
        networks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(networks)
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

    pub fn spawn_signal_listener(&self) -> tokio::sync::mpsc::Receiver<IwdEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let Some(conn) = self.conn.clone() else {
            return rx; // no connection, no events
        };
        let manager = self.clone();

        tokio::spawn(async move {
            let station_tx = tx.clone();
            let station_conn = conn.clone();
            let station_task = tokio::spawn(async move {
                loop {
                    let station_path_inner = match manager.get_station_path().await {
                        Ok(p) => p,
                        Err(_) => {
                            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                            continue;
                        }
                    };
                    let Ok(props) = PropertiesProxy::new(
                        &station_conn,
                        "net.connman.iwd",
                        station_path_inner.clone(),
                    )
                    .await
                    else {
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    };
                    if let Ok(mut stream) = props.receive_properties_changed().await {
                        while stream.next().await.is_some() {
                            let _ = station_tx.try_send(IwdEvent::ConnectedNetworkChanged);
                            let _ = station_tx.try_send(IwdEvent::NetworksChanged);
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            });

            let om_added_tx = tx.clone();
            let om_removed_tx = tx.clone();
            let om_added_conn = conn.clone();
            let om_removed_conn = conn.clone();

            let om_added_task = tokio::spawn(async move {
                loop {
                    let Ok(om) = ObjectManagerProxy::new(&om_added_conn, "net.connman.iwd", "/").await else {
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    };
                    if let Ok(mut stream) = om.receive_interfaces_added().await {
                        while stream.next().await.is_some() {
                            let _ = om_added_tx.try_send(IwdEvent::KnownNetworksChanged);
                            let _ = om_added_tx.try_send(IwdEvent::NetworksChanged);
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            });

            let om_removed_task = tokio::spawn(async move {
                loop {
                    let Ok(om) = ObjectManagerProxy::new(&om_removed_conn, "net.connman.iwd", "/").await else {
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    };
                    if let Ok(mut stream) = om.receive_interfaces_removed().await {
                        while stream.next().await.is_some() {
                            let _ = om_removed_tx.try_send(IwdEvent::KnownNetworksChanged);
                            let _ = om_removed_tx.try_send(IwdEvent::NetworksChanged);
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            });

            let _ = tokio::join!(station_task, om_added_task, om_removed_task);
        });

        rx
    }
}

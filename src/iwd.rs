//! D-Bus interface to iwd.
//!
//! Everything that talks to `net.connman.iwd` lives here:
//!
//! * reading state:    [`snapshot`]
//! * commands:         [`scan`], [`connect`], [`disconnect`], [`forget`]
//! * change signals:   [`watch`]  (drives the auto-refresh)
//! * credentials:      [`register_agent`] and the `Agent` impl
//!
//! API notes for iwd 3.x: connections are made with Network.Connect()
//! (Station.Connect was removed long ago); networks and their security
//! type come from the ObjectManager, which is also how iwctl reads them.

use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection, MatchRule, Message, Proxy};

use crate::app::{AgentQuery, AppEvent};

pub const IWD_SERVICE: &str = "net.connman.iwd";
/// Where our agent object lives on the bus.
pub const AGENT_PATH: &str = "/com/iwtui/agent";

const STATION_IFACE: &str = "net.connman.iwd.Station";
const NETWORK_IFACE: &str = "net.connman.iwd.Network";
const KNOWN_NETWORK_IFACE: &str = "net.connman.iwd.KnownNetwork";
const AGENT_MANAGER_IFACE: &str = "net.connman.iwd.AgentManager";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";

// ------------------------------------------------------------------ state

#[derive(Debug, Clone)]
pub struct NetworkEntry {
    pub path: OwnedObjectPath,
    pub name: String,
    /// Human readable, e.g. "WPA-PSK", "Open", "802.1X".
    pub security: String,
    /// Signal strength in dBm (only exposed by GetOrderedNetworks).
    pub signal: Option<i16>,
    pub connected: bool,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub station: OwnedObjectPath,
    pub state: String,
    pub scanning: bool,
    pub networks: Vec<NetworkEntry>,
}

// OwnedValue is not Clone in zbus 4, so properties are read by matching
// on the borrowed Value variants.
fn prop_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match &**props.get(key)? {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match &**props.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn prop_i16(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i16> {
    match &**props.get(key)? {
        Value::I16(v) => Some(*v),
        _ => None,
    }
}

fn prop_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match &**props.get(key)? {
        Value::ObjectPath(p) => Some(p.as_str().to_owned()),
        _ => None,
    }
}

fn pretty_security(raw: &str) -> String {
    match raw {
        "open" => "Open",
        "psk" => "WPA-PSK",
        "sae" => "WPA3",
        "owe" => "Enh. Open",
        "8021x" => "802.1X",
        "wep" => "WEP",
        other => other,
    }
    .to_owned()
}

type ManagedObjects =
    HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

async fn managed_objects(conn: &Connection) -> zbus::Result<ManagedObjects> {
    let om =
        Proxy::new(conn, IWD_SERVICE, "/net/connman/iwd", OBJECT_MANAGER_IFACE).await?;
    Ok(om.call("GetManagedObjects", &()).await?)
}

/// Everything the Wi-Fi screen shows, in one round of D-Bus calls.
/// Returns `None` when there is no station (radio off / no Wi-Fi device).
pub async fn snapshot(conn: &Connection) -> zbus::Result<Option<Snapshot>> {
    let objects = managed_objects(conn).await?;
    let Some(station) = objects
        .iter()
        .find(|(_, ifaces)| ifaces.contains_key(STATION_IFACE))
        .map(|(path, _)| path.clone())
    else {
        return Ok(None);
    };

    let station_props =
        Proxy::new(conn, IWD_SERVICE, station.clone(), PROPERTIES_IFACE).await?;
    let all: HashMap<String, OwnedValue> = station_props.call("GetAll", &STATION_IFACE).await?;
    let state = prop_str(&all, "State").unwrap_or_else(|| "unknown".to_owned());
    let scanning = prop_bool(&all, "Scanning").unwrap_or(false);
    let connected = prop_path(&all, "ConnectedNetwork").filter(|p| p != "/");

    let station_api =
        Proxy::new(conn, IWD_SERVICE, station.clone(), STATION_IFACE).await?;
    let ordered: Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)> =
        station_api.call("GetOrderedNetworks", &()).await?;

    let networks = ordered
        .into_iter()
        .map(|(path, scan_props)| {
            // Name/Signal come from the scan result; the security type
            // (and a Name fallback) from the Network object's own
            // properties, exactly like iwctl reads them.
            let object_props = objects
                .get(&path)
                .and_then(|ifaces| ifaces.get(NETWORK_IFACE));
            let name = prop_str(&scan_props, "Name")
                .or_else(|| object_props.and_then(|p| prop_str(p, "Name")))
                .unwrap_or_else(|| "<unknown>".to_owned());
            let security = object_props
                .and_then(|p| prop_str(p, "Type").or_else(|| prop_str(p, "Security")))
                .map(|raw| pretty_security(&raw))
                .unwrap_or_default();
            let connected_flag = connected.as_deref() == Some(path.as_str())
                || object_props
                    .and_then(|p| prop_bool(p, "Connected"))
                    .unwrap_or(false);
            NetworkEntry {
                signal: prop_i16(&scan_props, "Signal"),
                name,
                security,
                connected: connected_flag,
                path,
            }
        })
        .collect();

    Ok(Some(Snapshot { station, state, scanning, networks }))
}

// --------------------------------------------------------------- commands

pub async fn scan(conn: &Connection, station: &OwnedObjectPath) -> zbus::Result<()> {
    let p = Proxy::new(conn, IWD_SERVICE, station.clone(), STATION_IFACE).await?;
    p.call_method("Scan", &()).await?;
    Ok(())
}

/// Connect to `network` via Network.Connect.
///
/// If iwd needs a passphrase it will call our agent while this is in
/// flight, so callers must spawn this instead of awaiting it inline.
pub async fn connect(conn: &Connection, network: &OwnedObjectPath) -> zbus::Result<()> {
    let p = Proxy::new(conn, IWD_SERVICE, network.clone(), NETWORK_IFACE).await?;
    p.call_method("Connect", &()).await?;
    Ok(())
}

pub async fn disconnect(conn: &Connection, station: &OwnedObjectPath) -> zbus::Result<()> {
    let p = Proxy::new(conn, IWD_SERVICE, station.clone(), STATION_IFACE).await?;
    p.call_method("Disconnect", &()).await?;
    Ok(())
}

/// Delete the saved profile ("known network") behind a scanned network.
pub async fn forget(conn: &Connection, network: &OwnedObjectPath) -> zbus::Result<()> {
    let props = Proxy::new(conn, IWD_SERVICE, network.clone(), PROPERTIES_IFACE).await?;
    let known: Option<OwnedValue> =
        props.call("Get", &(NETWORK_IFACE, "KnownNetwork")).await.ok();
    let Some(known) = known else { return Ok(()) };
    let known_path = match &*known {
        Value::ObjectPath(p) => p.as_str().to_owned(),
        _ => return Ok(()), // not a known network; nothing to forget
    };
    let known_network =
        Proxy::new(conn, IWD_SERVICE, known_path, KNOWN_NETWORK_IFACE).await?;
    known_network.call_method("Delete", &()).await?;
    Ok(())
}

// ------------------------------------------------------------------ agent

struct Agent {
    events: mpsc::Sender<AppEvent>,
}

/// Implements net.connman.iwd.Agent: iwd calls these methods whenever it
/// needs credentials from the user; we forward them to the UI as events
/// and answer through the oneshot channel carried in the event.
#[interface(name = "net.connman.iwd.Agent")]
impl Agent {
    async fn release(&self) {}

    /// iwd no longer wants the answer it asked for.
    async fn cancel(&self, reason: String) {
        let _ = self.events.send(AppEvent::AgentCancelled(reason)).await;
    }

    async fn request_passphrase(&self, network: OwnedObjectPath) -> zbus::fdo::Result<String> {
        let (respond, answer) = oneshot::channel::<Option<String>>();
        self.events
            .send(AppEvent::AgentQuery(AgentQuery::Passphrase { network, respond }))
            .await
            .map_err(|_| zbus::fdo::Error::Failed("UI is gone".to_owned()))?;
        match answer.await {
            Ok(Some(passphrase)) => Ok(passphrase),
            _ => Err(zbus::fdo::Error::Failed("cancelled".to_owned())),
        }
    }
}

pub async fn register_agent(
    conn: &Connection,
    events: mpsc::Sender<AppEvent>,
) -> zbus::Result<()> {
    conn.object_server().at(AGENT_PATH, Agent { events }).await?;
    let manager =
        Proxy::new(conn, IWD_SERVICE, "/net/connman/iwd", AGENT_MANAGER_IFACE).await?;
    manager.call_method("RegisterAgent", &AGENT_PATH).await?;
    Ok(())
}

pub async fn unregister_agent(conn: &Connection) -> zbus::Result<()> {
    let manager =
        Proxy::new(conn, IWD_SERVICE, "/net/connman/iwd", AGENT_MANAGER_IFACE).await?;
    manager.call_method("UnregisterAgent", &AGENT_PATH).await?;
    Ok(())
}

// ---------------------------------------------------------------- signals

/// Translate every interesting iwd signal into `AppEvent::IwdChanged`.
/// The main loop debounces these and re-reads state, which is what
/// keeps the network list fresh without any manual refresh.
pub async fn watch(conn: Connection, events: mpsc::Sender<AppEvent>) -> zbus::Result<()> {
    let iwd_rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(IWD_SERVICE)?
        .build();
    let mut iwd_signals = zbus::MessageStream::for_match_rule(iwd_rule, &conn, None).await?;

    // Also notice iwd itself (re)starting on the bus.
    let ownership = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .build();
    let mut ownership_signals =
        zbus::MessageStream::for_match_rule(ownership, &conn, None).await?;

    loop {
        tokio::select! {
            message = iwd_signals.next() => match message {
                Some(Ok(msg)) => {
                    if signal_relevant(&msg) {
                        let _ = events.send(AppEvent::IwdChanged).await;
                    }
                }
                Some(Err(_)) => {}
                None => break,
            },
            message = ownership_signals.next() => match message {
                Some(Ok(msg)) => {
                    if let Ok((name, _, _)) =
                        msg.body().deserialize::<(String, String, String)>()
                    {
                        if name == IWD_SERVICE {
                            let _ = events.send(AppEvent::IwdChanged).await;
                        }
                    }
                }
                Some(Err(_)) => {}
                None => break,
            },
        }
    }
    Ok(())
}

fn signal_relevant(msg: &Message) -> bool {
    let interface = msg.header().interface().map(|i| i.as_str().to_owned());
    let member = msg.header().member().map(|m| m.as_str().to_owned());

    match (interface.as_deref(), member.as_deref()) {
        // networks appearing/disappearing after a scan
        (Some(STATION_IFACE), Some("NetworkAdded" | "NetworkRemoved")) => true,
        // device/station appearing or going away (radio toggle, hotplug)
        (
            Some("org.freedesktop.DBus.ObjectManager"),
            Some("InterfacesAdded" | "InterfacesRemoved"),
        ) => true,
        (Some(PROPERTIES_IFACE), Some("PropertiesChanged")) => properties_relevant(msg),
        _ => false,
    }
}

fn properties_relevant(msg: &Message) -> bool {
    let Ok((owner, changed, invalidated)) = msg
        .body()
        .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
    else {
        return false;
    };
    match owner.as_str() {
        // State: connected/disconnected/...; Scanning false => scan
        // results are in; ConnectedNetwork: we associated or roamed.
        STATION_IFACE => ["State", "Scanning", "ConnectedNetwork"].iter().any(|key| {
            changed.contains_key(*key) || invalidated.iter().any(|i| i.as_str() == *key)
        }),
        NETWORK_IFACE => changed.contains_key("Connected"),
        "net.connman.iwd.Device" | "net.connman.iwd.Adapter" => changed.contains_key("Powered"),
        _ => false,
    }
}

//! The application state machine: screens, focus, key handling, background
//! actions and the events that glue them together.
//!
//! Threading model:
//! * The UI loop never blocks on D-Bus. Background tasks fetch data or run
//!   actions and report back through [`AppEvent`]s on an mpsc channel.
//! * iwd's agent calls arrive on the zbus object server and are forwarded
//!   through a separate channel, polled non-blockingly each loop iteration.
//! * Every error that reaches the user goes through [`human_error`], so the
//!   interface only ever speaks human.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

use crate::agent::{AgentReply, AgentRequest, IwdAgent};
use crate::iwd::{AgentManagerProxy, AppKnownNetwork, AppNetwork, IwdEvent, IwdManager};
use crate::system;
use crate::{err, AppResult};

// ── human-readable errors ────────────────────────────────────────

/// Translate raw D-Bus / iwd / sudo error text into something a human
/// actually wants to read. Unknown errors pass through untouched.
fn human_error(raw: &str) -> String {
    // iwd's own D-Bus error names first (net.connman.iwd.Error.*).
    if raw.contains("NotConfigured") {
        return "No password is saved for that network — connect again and type it in.".into();
    }
    if raw.contains("NoAgent") {
        return "No password was provided for that network.".into();
    }
    if raw.contains("AlreadyProvisioned") {
        return "That network is already set up.".into();
    }
    if raw.contains("AlreadyConnected") {
        return "Already connected to that network.".into();
    }
    if raw.contains("NotConnected") {
        return "Not connected to any network.".into();
    }
    if raw.contains("NotFound") {
        return "That network is out of range — rescan and try again.".into();
    }
    if raw.contains("InProgress") || raw.contains("in progress") {
        return "Another operation is already running — give it a second.".into();
    }
    if raw.contains("Aborted") || raw.contains("Canceled") || raw.contains("Cancelled") {
        return "Cancelled.".into();
    }
    if raw.contains("OperationTimeout") {
        return "The network took too long to answer.".into();
    }
    if raw.contains("Error.Failed") {
        return "That did not work — try again.".into();
    }
    // sudo / privilege problems.
    if raw.contains("sudoers") {
        return "Your user is not allowed to use sudo — run iwtui as root instead.".into();
    }
    if raw.contains("incorrect password") || raw.contains("authentication failure") {
        return "Wrong password — try again.".into();
    }
    // D-Bus / system level problems.
    if raw.contains("ServiceUnknown") || raw.contains("was not provided by any .service") {
        return "iwd is not running — start it with: systemctl start iwd".into();
    }
    if raw.contains("AccessDenied")
        || raw.contains("PermissionDenied")
        || raw.contains("Permission denied")
    {
        return "Permission denied — iwd wants root or the netdev group for that.".into();
    }
    if raw.contains("timed out") || raw.contains("Timed out") {
        return "Timed out — iwd took too long to answer.".into();
    }
    if raw.contains("Busy") {
        return "iwd is busy — try again in a moment.".into();
    }
    if raw.contains("NotSupported") {
        return "Your hardware or driver does not support that.".into();
    }
    if raw.contains("InvalidFormat") {
        return "That value does not look right.".into();
    }
    if raw.contains("No such file or directory") || raw.contains("No such device") {
        return "The Wi-Fi device seems to have vanished — try again.".into();
    }
    raw.trim().to_string()
}

// ── menu + button enums ──────────────────────────────────────────

/// Items of the main menu, in nmtui order.
pub const MAIN_ITEMS: [&str; 4] = [
    "Edit a connection",
    "Activate a connection",
    "Set system hostname",
    "Quit",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    List,
    Buttons,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActivateButton {
    Rescan,
    ToggleConnection,
    Quit,
}

impl ActivateButton {
    pub const ALL: [Self; 3] = [Self::Rescan, Self::ToggleConnection, Self::Quit];

    /// Dynamic label depending on the connected state of the highlighted
    /// network; the UI calls this instead of a static label.
    pub fn label_for(self, connected: bool) -> &'static str {
        match self {
            Self::Rescan => "Rescan",
            Self::ToggleConnection => {
                if connected {
                    "Disconnect"
                } else {
                    "Connect"
                }
            }
            Self::Quit => "Quit",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditListButton {
    Add,
    Delete,
    Quit,
}

impl EditListButton {
    pub const ALL: [Self; 3] = [Self::Add, Self::Delete, Self::Quit];
    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Delete => "Delete",
            Self::Quit => "Quit",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditFormButton {
    Cancel,
    Forget,
    Save,
}

impl EditFormButton {
    pub const ALL: [Self; 3] = [Self::Cancel, Self::Forget, Self::Save];
    pub fn label(self) -> &'static str {
        match self {
            Self::Cancel => "Cancel",
            Self::Forget => "Forget",
            Self::Save => "Save",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostnameButton {
    Cancel,
    Set,
}

impl HostnameButton {
    pub const ALL: [Self; 2] = [Self::Cancel, Self::Set];
    pub fn label(self) -> &'static str {
        match self {
            Self::Cancel => "Cancel",
            Self::Set => "Set",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddHiddenButton {
    Cancel,
    Connect,
}

impl AddHiddenButton {
    pub const ALL: [Self; 2] = [Self::Cancel, Self::Connect];
    pub fn label(self) -> &'static str {
        match self {
            Self::Cancel => "Cancel",
            Self::Connect => "Connect",
        }
    }
}

/// Buttons of the root-password (hostname escalation) dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthButton {
    Cancel,
    Authenticate,
}

impl AuthButton {
    pub const ALL: [Self; 2] = [Self::Cancel, Self::Authenticate];
    pub fn label(self) -> &'static str {
        match self {
            Self::Cancel => "Cancel",
            Self::Authenticate => "Authenticate",
        }
    }
}

// ── dialogs & screens ────────────────────────────────────────────

/// Root-password prompt raised when `sethostname(2)` fails with EPERM —
/// the nmtui experience: ask for the password, never a permission error.
/// The elevated set runs in the background; `busy` freezes the dialog
/// while it is in flight.
pub struct RootAuth {
    pub pending_hostname: String,
    pub password: String,
    pub message: Option<String>,
    pub button: AuthButton,
    pub focus: Focus,
    pub busy: bool,
}

/// Credential prompts raised by the D-Bus agent while a connection attempt
/// is in flight. `prev_screen` is restored when the dialog closes.
pub enum AgentDialog {
    Passphrase {
        network_name: String,
        pass: String,
        reply_to: tokio::sync::oneshot::Sender<AgentReply<String>>,
        prev_screen: Box<Screen>,
    },
    UserPassword {
        network_name: String,
        user: String,
        pass: String,
        editing_user: bool,
        reply_to: tokio::sync::oneshot::Sender<AgentReply<String>>,
        prev_screen: Box<Screen>,
    },
    UserNameAndPassword {
        network_name: String,
        user: String,
        pass: String,
        editing_user: bool,
        reply_to: tokio::sync::oneshot::Sender<AgentReply<(String, String)>>,
        prev_screen: Box<Screen>,
    },
    PrivateKeyPassphrase {
        network_name: String,
        pass: String,
        reply_to: tokio::sync::oneshot::Sender<AgentReply<String>>,
        prev_screen: Box<Screen>,
    },
}

impl AgentDialog {
    pub fn prompt(&self) -> String {
        match self {
            AgentDialog::Passphrase { network_name, .. }
            | AgentDialog::UserPassword { network_name, .. } => {
                // The username is shown in its own field for UserPassword,
                // so the prompt stays a single line (it renders in a 1-row
                // slot).
                format!("Passwords are required to connect to '{network_name}'")
            }
            AgentDialog::UserNameAndPassword { network_name, .. } => {
                format!("Credentials are required to connect to '{network_name}'")
            }
            AgentDialog::PrivateKeyPassphrase { network_name, .. } => {
                format!("Private key passphrase required for '{network_name}'")
            }
        }
    }

    pub fn prev_screen_ref(&self) -> &Screen {
        match self {
            AgentDialog::Passphrase { prev_screen, .. }
            | AgentDialog::UserPassword { prev_screen, .. }
            | AgentDialog::UserNameAndPassword { prev_screen, .. }
            | AgentDialog::PrivateKeyPassphrase { prev_screen, .. } => prev_screen,
        }
    }

    fn take_prev_screen(&mut self) -> Box<Screen> {
        let dummy = Box::new(Screen::Main(0));
        match self {
            AgentDialog::Passphrase { prev_screen, .. }
            | AgentDialog::UserPassword { prev_screen, .. }
            | AgentDialog::UserNameAndPassword { prev_screen, .. }
            | AgentDialog::PrivateKeyPassphrase { prev_screen, .. } => {
                std::mem::replace(prev_screen, dummy)
            }
        }
    }

    fn cancel(self) {
        match self {
            AgentDialog::Passphrase { reply_to, .. }
            | AgentDialog::UserPassword { reply_to, .. }
            | AgentDialog::PrivateKeyPassphrase { reply_to, .. } => {
                let _ = reply_to.send(AgentReply::Cancelled);
            }
            AgentDialog::UserNameAndPassword { reply_to, .. } => {
                let _ = reply_to.send(AgentReply::Cancelled);
            }
        }
    }
}

pub struct EditForm {
    /// Identity of the network being edited (stable across list refreshes —
    /// index-based tracking broke whenever the list reordered underneath us).
    pub net_path: OwnedObjectPath,
    pub auto_connect: bool,
    pub button: EditFormButton,
    pub focus: Focus,
}

pub struct AddHiddenForm {
    pub ssid: String,
    pub button: AddHiddenButton,
    pub focus: Focus,
}

pub enum Screen {
    Main(usize),
    Activate {
        list_idx: usize,
        button: ActivateButton,
        focus: Focus,
    },
    EditList {
        list_idx: usize,
        button: EditListButton,
        focus: Focus,
    },
    EditForm(EditForm),
    AddHidden(AddHiddenForm),
    SetHostname {
        input: String,
        button: HostnameButton,
        focus: Focus,
    },
    RootAuth(RootAuth),
    AgentDialog(AgentDialog),
    Error(String),
}

/// Events produced by background tasks, consumed by the UI loop.
pub enum AppEvent {
    /// Result of a user-initiated action. `success` is shown in the status
    /// bar on success (much more useful than a generic "done" message).
    ActionResult {
        result: Result<(), String>,
        success: String,
    },
    /// Outcome of the sudo-elevated hostname set. Carries the hostname so
    /// stale results (user backed out mid-auth) can be detected.
    HostnameSet {
        name: String,
        result: Result<(), system::HostnameSetError>,
    },
    NetworksUpdated(Vec<AppNetwork>),
    KnownNetworksUpdated(Vec<AppKnownNetwork>),
    /// A background fetch or scan failed; shown in the status bar so
    /// failures are never silent.
    FetchFailed(String),
    StationStateUpdated(String),
}

// ── the app ──────────────────────────────────────────────────────

pub struct App {
    pub screen: Screen,
    pub iwd_manager: IwdManager,
    pub networks: Vec<AppNetwork>,
    pub known_networks: Vec<AppKnownNetwork>,
    pub hostname: String,
    pub device_name: Option<String>,
    pub wifi_powered: bool,
    pub should_quit: bool,
    pub agent_rx: mpsc::Receiver<AgentRequest>,
    pub iwd_events: mpsc::Receiver<IwdEvent>,
    pub app_rx: mpsc::Receiver<AppEvent>,
    pub app_tx: mpsc::Sender<AppEvent>,
    pub status_message: Option<String>,
    pub cancel_flag: Arc<AtomicBool>,
    pub error_prev_screen: Option<Box<Screen>>,
    /// Where to return when the Add-Hidden screen closes (it can be opened
    /// from both Edit and Activate).
    pub add_hidden_prev: Option<Box<Screen>>,
    /// Help overlay (toggled with `?` on non-text screens).
    pub show_help: bool,
    /// Password visibility inside agent dialogs (toggled with Ctrl+R).
    pub show_password: bool,
    pub station_state: String,
    /// Last periodic data refresh (see `poll_iwd_events`).
    pub last_refresh: Instant,
}

impl App {
    pub async fn new() -> App {
        let mut init_error: Option<String> = None;
        let conn = match Connection::system().await {
            Ok(c) => Some(c),
            Err(e) => {
                init_error = Some(format!(
                    "Could not connect to the system D-Bus.\n\n\
                     iwtui talks to iwd over D-Bus, so without it there is\n\
                     nothing to show. Is D-Bus running?\n\n\
                     Technical detail: {e}"
                ));
                None
            }
        };

        let iwd_manager = IwdManager::new(conn.clone());
        if conn.is_some() {
            // Non-fatal: the station may appear later (radio powered on),
            // `get_station_path` re-resolves dynamically in that case.
            if let Err(e) =
                tokio::time::timeout(Duration::from_secs(5), iwd_manager.init_station_path())
                    .await
                    .unwrap_or_else(|_| Err(err("init_station_path: timed out after 5s")))
            {
                eprintln!("iwtui: station init: {e}");
            }
        }

        let (agent_tx, agent_rx) = mpsc::channel(10);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (app_tx, app_rx) = mpsc::channel(64);

        // Register the agent. Missing iwd is degraded-mode (browse-only),
        // not a fatal error screen: the user can still see the UI and quit
        // cleanly. Only a missing system bus is fatal.
        let mut init_status: Option<String> = None;
        if let Some(conn) = conn.clone() {
            let agent = IwdAgent {
                tx: agent_tx,
                conn: conn.clone(),
                cancel_flag: cancel_flag.clone(),
            };
            match conn
                .object_server()
                .at("/net/connman/iwd/agent", agent)
                .await
            {
                Ok(true) => {}
                Ok(false) => init_status = Some("Could not register the password agent".into()),
                Err(e) => {
                    init_status = Some(human_error(&format!(
                        "Could not register the password agent: {e}"
                    )))
                }
            }
            if init_status.is_none() {
                match AgentManagerProxy::new(&conn).await {
                    Ok(agent_manager) => {
                        let agent_path = OwnedObjectPath::try_from("/net/connman/iwd/agent")
                            .expect("static path is valid");
                        if let Err(e) = agent_manager.register_agent(&agent_path).await {
                            init_status =
                                Some(human_error(&format!("Agent registration failed: {e}")));
                        }
                    }
                    Err(_) => {
                        init_status =
                            Some("iwd is not running — start it with: systemctl start iwd".into())
                    }
                }
            }
        }

        let mut iwd_events = if conn.is_some() {
            iwd_manager.spawn_signal_listener()
        } else {
            mpsc::channel(10).1
        };

        // Fetch initial data; surface errors in the status bar instead of
        // silently defaulting to empty lists.
        let mut fetch_status: Option<String> = None;
        let networks =
            match tokio::time::timeout(Duration::from_secs(5), iwd_manager.get_networks()).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    fetch_status = Some(human_error(&e.to_string()));
                    Vec::new()
                }
                Err(_) => {
                    fetch_status = Some("iwd took too long to answer at startup".into());
                    Vec::new()
                }
            };
        let known_networks =
            match tokio::time::timeout(Duration::from_secs(5), iwd_manager.get_known_networks())
                .await
            {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    if fetch_status.is_none() {
                        fetch_status = Some(human_error(&e.to_string()));
                    }
                    Vec::new()
                }
                Err(_) => {
                    if fetch_status.is_none() {
                        fetch_status = Some("iwd took too long to answer at startup".into());
                    }
                    Vec::new()
                }
            };

        let hostname = system::get_hostname().unwrap_or_else(|_| "localhost".to_string());
        let station_state = if conn.is_some() {
            tokio::time::timeout(Duration::from_secs(5), iwd_manager.get_station_state())
                .await
                .unwrap_or(Ok("unknown".to_string()))
                .unwrap_or_else(|_| "unknown".to_string())
        } else {
            "unknown".to_string()
        };
        let device_name =
            tokio::time::timeout(Duration::from_secs(5), iwd_manager.get_device_name())
                .await
                .unwrap_or(None);
        let wifi_powered =
            tokio::time::timeout(Duration::from_secs(5), iwd_manager.is_wifi_powered())
                .await
                .unwrap_or(Ok(true))
                .unwrap_or(true);

        // If the list came back empty, start a scan right away so the user
        // never has to press Rescan manually. "Already in progress" is fine.
        if networks.is_empty() && conn.is_some() {
            let manager = iwd_manager.clone();
            let tx = app_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = manager.trigger_scan().await {
                    let msg = e.to_string();
                    if !is_scan_in_progress(&msg) {
                        let _ = tx
                            .send(AppEvent::FetchFailed(format!(
                                "Scan: {}",
                                human_error(&msg)
                            )))
                            .await;
                    }
                }
            });
        }

        let status_message = init_status.or(fetch_status).or_else(|| {
            if networks.is_empty() && conn.is_some() {
                Some("Scanning...".into())
            } else {
                None
            }
        });

        // Clear any events queued during startup.
        while iwd_events.try_recv().is_ok() {}

        let mut screen = Screen::Main(0);
        if let Some(e) = init_error.take() {
            screen = Screen::Error(e);
        }

        App {
            screen,
            iwd_manager,
            networks,
            known_networks,
            hostname,
            device_name,
            wifi_powered,
            should_quit: false,
            agent_rx,
            iwd_events,
            app_rx,
            app_tx,
            status_message,
            cancel_flag,
            error_prev_screen: None,
            add_hidden_prev: None,
            show_help: false,
            show_password: false,
            station_state,
            last_refresh: Instant::now(),
        }
    }

    pub async fn shutdown(&self) {
        if let Some(conn) = &self.iwd_manager.conn {
            if let Ok(agent_manager) = AgentManagerProxy::new(conn).await {
                let agent_path = OwnedObjectPath::try_from("/net/connman/iwd/agent")
                    .expect("static path is valid");
                let _ = agent_manager.unregister_agent(&agent_path).await;
            }
        }
    }

    // ------------------------------------------------------------------
    // Selection helpers (lists can shrink between refreshes — keep the
    // highlight anchored to the same network instead of an out-of-range
    // index).
    // ------------------------------------------------------------------

    pub fn known_index_by_path(&self, path: &OwnedObjectPath) -> Option<usize> {
        self.known_networks.iter().position(|k| k.path == *path)
    }

    fn apply_networks(&mut self, nets: Vec<AppNetwork>) {
        let sel = match &self.screen {
            Screen::Activate { list_idx, .. } => self
                .networks
                .get(*list_idx)
                .map(|n| n.path.as_str().to_string()),
            _ => None,
        };
        self.networks = nets;
        if let Screen::Activate { list_idx, .. } = &mut self.screen {
            *list_idx = idx_by_path(&self.networks, sel.as_deref(), |n| n.path.as_str());
        }
    }

    fn apply_known_networks(&mut self, known: Vec<AppKnownNetwork>) {
        let sel = match &self.screen {
            Screen::EditList { list_idx, .. } => self
                .known_networks
                .get(*list_idx)
                .map(|n| n.path.as_str().to_string()),
            _ => None,
        };
        self.known_networks = known;
        if let Screen::EditList { list_idx, .. } = &mut self.screen {
            *list_idx = idx_by_path(&self.known_networks, sel.as_deref(), |n| n.path.as_str());
        }
    }

    // ------------------------------------------------------------------
    // Agent plumbing
    // ------------------------------------------------------------------

    /// Non-blocking check for iwd-initiated Cancel. If set, dismiss whatever
    /// agent dialog is currently on screen.
    pub fn poll_cancel_flag(&mut self) {
        if self.cancel_flag.swap(false, Ordering::SeqCst) {
            self.dismiss_agent_dialog();
        }
    }

    pub fn dismiss_agent_dialog(&mut self) {
        if let Screen::AgentDialog(dialog) = std::mem::replace(&mut self.screen, Screen::Main(0)) {
            let mut d = dialog;
            let prev = *d.take_prev_screen();
            d.cancel();
            self.screen = prev;
        }
    }

    pub fn poll_agent_requests(&mut self) {
        while let Ok(req) = self.agent_rx.try_recv() {
            // If a dialog is already on screen, cancel it first and show the
            // new request. iwd legitimately retries (e.g. wrong password) by
            // sending a second Request* call while the first is pending.
            if matches!(self.screen, Screen::AgentDialog(_)) {
                self.dismiss_agent_dialog();
            }
            self.show_password = false;
            let prev = Box::new(std::mem::replace(&mut self.screen, Screen::Main(0)));
            match req {
                AgentRequest::RequestPassphrase {
                    network_name,
                    reply_to,
                } => {
                    self.screen = Screen::AgentDialog(AgentDialog::Passphrase {
                        network_name,
                        pass: String::new(),
                        reply_to,
                        prev_screen: prev,
                    });
                }
                AgentRequest::RequestUserPassword {
                    network_name,
                    user,
                    reply_to,
                } => {
                    self.screen = Screen::AgentDialog(AgentDialog::UserPassword {
                        network_name,
                        user,
                        pass: String::new(),
                        editing_user: false,
                        reply_to,
                        prev_screen: prev,
                    });
                }
                AgentRequest::RequestUserNameAndPassword {
                    network_name,
                    reply_to,
                } => {
                    self.screen = Screen::AgentDialog(AgentDialog::UserNameAndPassword {
                        network_name,
                        user: String::new(),
                        pass: String::new(),
                        editing_user: true,
                        reply_to,
                        prev_screen: prev,
                    });
                }
                AgentRequest::RequestPrivateKeyPassphrase {
                    network_name,
                    reply_to,
                } => {
                    self.screen = Screen::AgentDialog(AgentDialog::PrivateKeyPassphrase {
                        network_name,
                        pass: String::new(),
                        reply_to,
                        prev_screen: prev,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Event pumps (all non-blocking)
    // ------------------------------------------------------------------

    pub fn poll_app_events(&mut self) {
        while let Ok(ev) = self.app_rx.try_recv() {
            match ev {
                AppEvent::ActionResult { result, success } => match result {
                    Ok(()) => {
                        self.status_message = Some(success);
                        self.refresh_after_action();
                    }
                    Err(e) => {
                        if is_cancellation(&e) {
                            // iwd reports the agent cancellation as an
                            // error; show it as a calm status instead of a
                            // scary modal.
                            self.status_message = Some("Cancelled".into());
                            self.refresh_after_action();
                        } else {
                            let msg = human_error(&e);
                            self.error_prev_screen = Some(Box::new(std::mem::replace(
                                &mut self.screen,
                                Screen::Main(0),
                            )));
                            self.screen = Screen::Error(msg);
                        }
                    }
                },
                AppEvent::HostnameSet { name, result } => match result {
                    Ok(()) => {
                        self.hostname = name;
                        self.status_message = Some("Hostname updated".into());
                        // Leave the auth dialog only if it is still on
                        // screen (stale results are silently ignored).
                        if matches!(self.screen, Screen::RootAuth(_)) {
                            self.screen = Screen::Main(2);
                        }
                    }
                    Err(e) => {
                        let is_auth = matches!(e, system::HostnameSetError::AuthFailed);
                        let msg = human_error(&e.to_string());
                        if let Screen::RootAuth(auth) = &mut self.screen {
                            if auth.pending_hostname == name && auth.busy {
                                auth.busy = false;
                                auth.password.clear();
                                auth.focus = Focus::List;
                                auth.message = Some(if is_auth {
                                    "Wrong password — try again.".into()
                                } else {
                                    msg
                                });
                                continue;
                            }
                        }
                        // Dialog already closed: keep it calm, inform only.
                        self.status_message = Some(if is_auth {
                            "Hostname authentication failed".into()
                        } else {
                            format!("Hostname: {msg}")
                        });
                    }
                },
                AppEvent::NetworksUpdated(nets) => self.apply_networks(nets),
                AppEvent::KnownNetworksUpdated(known) => self.apply_known_networks(known),
                AppEvent::FetchFailed(e) => self.status_message = Some(human_error(&e)),
                AppEvent::StationStateUpdated(state) => self.station_state = state,
            }
        }
    }

    /// Non-blocking. Reacts to iwd signals AND refreshes periodically as a
    /// safety net, so the list always eventually populates after a scan.
    pub fn poll_iwd_events(&mut self) {
        let mut need_networks = false;
        let mut need_known = false;
        while let Ok(ev) = self.iwd_events.try_recv() {
            match ev {
                IwdEvent::NetworksChanged => need_networks = true,
                IwdEvent::ConnectedNetworkChanged => need_networks = true,
                IwdEvent::KnownNetworksChanged => {
                    need_known = true;
                    need_networks = true;
                }
            }
        }

        if self.iwd_manager.conn.is_some() && self.last_refresh.elapsed() >= Duration::from_secs(5)
        {
            self.last_refresh = Instant::now();
            need_networks = true;
            need_known = true;
        }

        if need_networks || need_known {
            self.refresh_data(need_networks, need_known);
        }
    }

    // ------------------------------------------------------------------
    // Input dispatch
    // ------------------------------------------------------------------

    pub async fn handle_event(&mut self, ev: Event) -> bool {
        if let Event::Key(key) = ev {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.should_quit = true;
                return true;
            }

            // Help overlay swallows the next key.
            if self.show_help {
                if matches!(
                    key.code,
                    KeyCode::Esc
                        | KeyCode::Enter
                        | KeyCode::Char('?')
                        | KeyCode::Char('q')
                        | KeyCode::Char('h')
                ) {
                    self.show_help = false;
                }
                return self.should_quit;
            }

            // '?' toggles help everywhere text input is not active.
            let text_screen = matches!(
                self.screen,
                Screen::AddHidden(_)
                    | Screen::SetHostname { .. }
                    | Screen::RootAuth(_)
                    | Screen::AgentDialog(_)
            );
            if key.code == KeyCode::Char('?') && key.modifiers.is_empty() && !text_screen {
                self.show_help = true;
                return self.should_quit;
            }

            let current = std::mem::replace(&mut self.screen, Screen::Main(0));
            match current {
                Screen::AgentDialog(dialog) => self.handle_agent_dialog_key(dialog, key),
                Screen::Main(idx) => self.handle_main_key(key, idx),
                Screen::Activate {
                    list_idx,
                    button,
                    focus,
                } => self.handle_activate_key(key, list_idx, button, focus).await,
                Screen::EditList {
                    list_idx,
                    button,
                    focus,
                } => {
                    self.handle_edit_list_key(key, list_idx, button, focus)
                        .await
                }
                Screen::EditForm(form) => self.handle_edit_form_key(key, form).await,
                Screen::AddHidden(form) => self.handle_add_hidden_key(key, form).await,
                Screen::SetHostname {
                    input,
                    button,
                    focus,
                } => self.handle_hostname_key(key, input, button, focus),
                Screen::RootAuth(auth) => self.handle_root_auth_key(auth, key),
                Screen::Error(msg) => {
                    if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                        self.screen = *self
                            .error_prev_screen
                            .take()
                            .unwrap_or(Box::new(Screen::Main(0)));
                    } else {
                        self.screen = Screen::Error(msg);
                    }
                }
            }
        }
        self.should_quit
    }

    // ------------------------------------------------------------------
    // Add-hidden screen bookkeeping (opened from two places)
    // ------------------------------------------------------------------

    pub fn open_add_hidden(&mut self) {
        let prev = std::mem::replace(&mut self.screen, Screen::Main(0));
        self.add_hidden_prev = Some(Box::new(prev));
        self.screen = Screen::AddHidden(AddHiddenForm {
            ssid: String::new(),
            button: AddHiddenButton::Connect,
            focus: Focus::List,
        });
    }

    pub fn close_add_hidden(&mut self) {
        self.screen = match self.add_hidden_prev.take() {
            Some(prev) => *prev,
            None => Screen::EditList {
                list_idx: 0,
                button: EditListButton::Quit,
                focus: Focus::List,
            },
        };
    }
}

/// Find the index of `sel` (by path) inside `items`, clamped to a valid
/// range. Falls back to 0 when the selection vanished.
fn idx_by_path<T>(items: &[T], sel: Option<&str>, key: impl Fn(&T) -> &str) -> usize {
    if items.is_empty() {
        return 0;
    }
    sel.and_then(|p| items.iter().position(|i| key(i) == p))
        .unwrap_or(0)
}

/// iwd reports "scan already running" either as its own `InProgress` D-Bus
/// error or as free text — both are fine, not a failure.
fn is_scan_in_progress(msg: &str) -> bool {
    msg.contains("InProgress") || msg.contains("in progress")
}

/// iwd reports the agent cancellation as a D-Bus error containing
/// "Canceled"/"Cancelled"/"Aborted".
fn is_cancellation(msg: &str) -> bool {
    msg.contains("ancel") || msg.contains("borted")
}

// ── key handling ─────────────────────────────────────────────────
// Conventions:
// * `j/k` + arrows navigate lists, `h/l` + arrows move between buttons.
// * `Tab` toggles list <-> buttons focus.
// * `Esc` (or `q` on list screens) goes back.
// * Text input screens only accept unmodified chars — Ctrl/Alt-modified
//   chars never leak into text buffers.

impl App {
    pub fn handle_main_key(&mut self, key: KeyEvent, idx: usize) {
        let n = MAIN_ITEMS.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.screen = Screen::Main((idx + 1) % n),
            KeyCode::Up | KeyCode::Char('k') => self.screen = Screen::Main((idx + n - 1) % n),
            KeyCode::Home => self.screen = Screen::Main(0),
            KeyCode::End => self.screen = Screen::Main(n - 1),
            KeyCode::Enter => match idx {
                0 => {
                    self.screen = Screen::EditList {
                        list_idx: 0,
                        button: EditListButton::Quit,
                        focus: Focus::List,
                    }
                }
                1 => {
                    self.screen = Screen::Activate {
                        list_idx: 0,
                        button: ActivateButton::ToggleConnection,
                        focus: Focus::List,
                    };
                    if self.networks.is_empty() {
                        self.spawn_scan();
                    }
                }
                2 => {
                    self.screen = Screen::SetHostname {
                        input: self.hostname.clone(),
                        button: HostnameButton::Set,
                        focus: Focus::List,
                    }
                }
                3 => self.should_quit = true,
                _ => {}
            },
            _ => {}
        }
    }

    pub async fn handle_activate_key(
        &mut self,
        key: KeyEvent,
        mut list_idx: usize,
        mut button: ActivateButton,
        mut focus: Focus,
    ) {
        let n_len = self.networks.len();
        let b_len = ActivateButton::ALL.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Main(1);
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                focus = if focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if focus == Focus::List {
                    if n_len > 0 {
                        list_idx = (list_idx + 1) % n_len;
                    }
                } else {
                    focus = Focus::List;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if focus == Focus::List {
                    if n_len > 0 {
                        list_idx = (list_idx + n_len - 1) % n_len;
                    }
                } else {
                    focus = Focus::List;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if focus == Focus::Buttons {
                    button = ActivateButton::ALL[(button as usize + b_len - 1) % b_len];
                } else {
                    button = ActivateButton::ALL[b_len - 1];
                    focus = Focus::Buttons;
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if focus == Focus::Buttons {
                    button = ActivateButton::ALL[(button as usize + 1) % b_len];
                } else {
                    button = ActivateButton::ALL[0];
                    focus = Focus::Buttons;
                }
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => self.spawn_scan(),
            KeyCode::Char('n') if key.modifiers.is_empty() => {
                self.open_add_hidden();
                return;
            }
            KeyCode::Char('p') if key.modifiers.is_empty() => self.toggle_wifi_power(),
            KeyCode::Enter => match focus {
                Focus::List => {
                    if self.station_state == "scanning" {
                        self.status_message = Some("Scan in progress, please wait...".into());
                    } else {
                        self.toggle_connection(list_idx);
                    }
                }
                Focus::Buttons => match button {
                    ActivateButton::ToggleConnection => {
                        if self.station_state == "scanning" {
                            self.status_message = Some("Scan in progress, please wait...".into());
                        } else {
                            self.toggle_connection(list_idx);
                        }
                    }
                    ActivateButton::Rescan => self.spawn_scan(),
                    ActivateButton::Quit => {
                        self.screen = Screen::Main(1);
                        return;
                    }
                },
            },
            _ => {}
        }
        self.screen = Screen::Activate {
            list_idx,
            button,
            focus,
        };
    }

    pub async fn handle_edit_list_key(
        &mut self,
        key: KeyEvent,
        mut list_idx: usize,
        mut button: EditListButton,
        mut focus: Focus,
    ) {
        let n_len = self.known_networks.len();
        let b_len = EditListButton::ALL.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Main(0);
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                focus = if focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if focus == Focus::List {
                    if n_len > 0 {
                        list_idx = (list_idx + 1) % n_len;
                    }
                } else {
                    focus = Focus::List;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if focus == Focus::List {
                    if n_len > 0 {
                        list_idx = (list_idx + n_len - 1) % n_len;
                    }
                } else {
                    focus = Focus::List;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if focus == Focus::Buttons {
                    button = EditListButton::ALL[(button as usize + b_len - 1) % b_len];
                } else {
                    button = EditListButton::ALL[b_len - 1];
                    focus = Focus::Buttons;
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if focus == Focus::Buttons {
                    button = EditListButton::ALL[(button as usize + 1) % b_len];
                } else {
                    button = EditListButton::ALL[0];
                    focus = Focus::Buttons;
                }
            }
            KeyCode::Delete => {
                if let Some(net) = self.known_networks.get(list_idx).cloned() {
                    self.forget_network(net.path, net.name);
                }
            }
            KeyCode::Enter => match focus {
                Focus::List => {
                    if let Some(net) = self.known_networks.get(list_idx).cloned() {
                        self.screen = Screen::EditForm(EditForm {
                            net_path: net.path,
                            auto_connect: net.auto_connect,
                            button: EditFormButton::Save,
                            focus: Focus::List,
                        });
                        return;
                    }
                }
                Focus::Buttons => match button {
                    EditListButton::Add => {
                        self.open_add_hidden();
                        return;
                    }
                    EditListButton::Delete => {
                        if let Some(net) = self.known_networks.get(list_idx).cloned() {
                            self.forget_network(net.path, net.name);
                        }
                    }
                    EditListButton::Quit => {
                        self.screen = Screen::Main(0);
                        return;
                    }
                },
            },
            _ => {}
        }
        self.screen = Screen::EditList {
            list_idx,
            button,
            focus,
        };
    }

    pub async fn handle_edit_form_key(&mut self, key: KeyEvent, mut form: EditForm) {
        let b_len = EditFormButton::ALL.len();
        match key.code {
            KeyCode::Esc => {
                self.screen = self.edit_list_screen(&form);
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                form.focus = if form.focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => {
                form.focus = if form.focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if form.focus == Focus::Buttons {
                    form.button = EditFormButton::ALL[(form.button as usize + b_len - 1) % b_len];
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if form.focus == Focus::Buttons {
                    form.button = EditFormButton::ALL[(form.button as usize + 1) % b_len];
                }
            }
            KeyCode::Char(' ') if form.focus == Focus::List => {
                form.auto_connect = !form.auto_connect;
            }
            KeyCode::Enter => {
                if form.focus == Focus::List {
                    form.focus = Focus::Buttons;
                    self.screen = Screen::EditForm(form);
                    return;
                }
                match form.button {
                    EditFormButton::Cancel => {
                        self.screen = self.edit_list_screen(&form);
                        return;
                    }
                    EditFormButton::Save => {
                        self.save_auto_connect(form.net_path.clone(), form.auto_connect);
                        self.screen = self.edit_list_screen(&form);
                        return;
                    }
                    EditFormButton::Forget => {
                        self.forget_network(form.net_path.clone(), self.known_name(&form));
                        self.screen = self.edit_list_screen(&form);
                        return;
                    }
                }
            }
            _ => {}
        }
        self.screen = Screen::EditForm(form);
    }

    /// Name of the network being edited (best effort, for status messages).
    fn known_name(&self, form: &EditForm) -> String {
        self.known_index_by_path(&form.net_path)
            .and_then(|i| self.known_networks.get(i))
            .map(|n| n.name.clone())
            .unwrap_or_default()
    }

    fn edit_list_screen(&self, form: &EditForm) -> Screen {
        let idx = self.known_index_by_path(&form.net_path).unwrap_or(0);
        Screen::EditList {
            list_idx: idx,
            button: EditListButton::Quit,
            focus: Focus::List,
        }
    }

    pub async fn handle_add_hidden_key(&mut self, key: KeyEvent, mut form: AddHiddenForm) {
        let b_len = AddHiddenButton::ALL.len();
        match key.code {
            KeyCode::Esc => {
                self.close_add_hidden();
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                form.focus = if form.focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Down | KeyCode::Up => {
                form.focus = if form.focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Char('j') | KeyCode::Char('k') => {
                // In the SSID field these are plain text; on the buttons
                // they navigate.
                if form.focus == Focus::Buttons {
                    form.focus = Focus::List;
                } else if let Some(c) = text_char(&key) {
                    form.ssid.push(c);
                }
            }
            KeyCode::Left => {
                if form.focus == Focus::Buttons {
                    form.button = AddHiddenButton::ALL[(form.button as usize + b_len - 1) % b_len];
                }
            }
            KeyCode::Char('h') => {
                if form.focus == Focus::Buttons {
                    form.button = AddHiddenButton::ALL[(form.button as usize + b_len - 1) % b_len];
                } else if let Some(c) = text_char(&key) {
                    form.ssid.push(c);
                }
            }
            KeyCode::Right => {
                if form.focus == Focus::Buttons {
                    form.button = AddHiddenButton::ALL[(form.button as usize + 1) % b_len];
                }
            }
            KeyCode::Char('l') => {
                if form.focus == Focus::Buttons {
                    form.button = AddHiddenButton::ALL[(form.button as usize + 1) % b_len];
                } else if let Some(c) = text_char(&key) {
                    form.ssid.push(c);
                }
            }
            KeyCode::Enter => {
                if form.focus == Focus::List {
                    form.focus = Focus::Buttons;
                    self.screen = Screen::AddHidden(form);
                    return;
                }
                match form.button {
                    AddHiddenButton::Cancel => {
                        self.close_add_hidden();
                        return;
                    }
                    AddHiddenButton::Connect => {
                        if form.ssid.is_empty() {
                            self.status_message =
                                Some("Type the name of the hidden network first".into());
                        } else {
                            let ssid = form.ssid.clone();
                            self.connect_hidden(ssid);
                            // The result will land on the Activate screen,
                            // where connection progress is visible.
                            self.add_hidden_prev = None;
                            self.screen = Screen::Activate {
                                list_idx: 0,
                                button: ActivateButton::ToggleConnection,
                                focus: Focus::List,
                            };
                        }
                        return;
                    }
                }
            }
            KeyCode::Backspace | KeyCode::Delete => {
                if form.focus == Focus::List {
                    form.ssid.pop();
                }
            }
            _ => {
                if let Some(c) = text_char(&key) {
                    if form.focus == Focus::List {
                        form.ssid.push(c);
                    }
                }
            }
        }
        self.screen = Screen::AddHidden(form);
    }

    pub fn handle_hostname_key(
        &mut self,
        key: KeyEvent,
        mut input: String,
        mut button: HostnameButton,
        mut focus: Focus,
    ) {
        let b_len = HostnameButton::ALL.len();
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::Main(2);
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                focus = if focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Down | KeyCode::Up => {
                focus = if focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Char('j') | KeyCode::Char('k') => {
                // In the field these are plain text (nmtui behaviour);
                // on the buttons they navigate.
                if focus == Focus::Buttons {
                    focus = Focus::List;
                } else if let Some(c) = text_char(&key) {
                    input.push(c);
                }
            }
            KeyCode::Left => {
                if focus == Focus::Buttons {
                    button = HostnameButton::ALL[(button as usize + b_len - 1) % b_len];
                }
            }
            KeyCode::Char('h') => {
                if focus == Focus::Buttons {
                    button = HostnameButton::ALL[(button as usize + b_len - 1) % b_len];
                } else if let Some(c) = text_char(&key) {
                    input.push(c);
                }
            }
            KeyCode::Right => {
                if focus == Focus::Buttons {
                    button = HostnameButton::ALL[(button as usize + 1) % b_len];
                }
            }
            KeyCode::Char('l') => {
                if focus == Focus::Buttons {
                    button = HostnameButton::ALL[(button as usize + 1) % b_len];
                } else if let Some(c) = text_char(&key) {
                    input.push(c);
                }
            }
            KeyCode::Enter => {
                if focus == Focus::List {
                    focus = Focus::Buttons;
                } else {
                    match button {
                        HostnameButton::Cancel => {
                            self.screen = Screen::Main(2);
                            return;
                        }
                        HostnameButton::Set => {
                            // Invalid input never becomes a modal error:
                            // the screen already shows it in red inline.
                            if let Err(msg) = system::validate_hostname(&input) {
                                self.status_message = Some(msg);
                                // fall through: stay on the hostname screen
                            } else {
                                // Direct attempt; on EPERM this opens the
                                // root-password dialog (submit_hostname
                                // sets self.screen in every branch).
                                self.submit_hostname(input);
                                return;
                            }
                        }
                    }
                }
            }
            KeyCode::Backspace | KeyCode::Delete => {
                if focus == Focus::List {
                    input.pop();
                }
            }
            _ => {
                if let Some(c) = text_char(&key) {
                    if focus == Focus::List {
                        input.push(c);
                    }
                }
            }
        }
        self.screen = Screen::SetHostname {
            input,
            button,
            focus,
        };
    }

    pub fn handle_root_auth_key(&mut self, mut auth: RootAuth, key: KeyEvent) {
        // The elevated set is running: freeze input until the result
        // arrives (it is a ≤20 s operation).
        if auth.busy {
            self.screen = Screen::RootAuth(auth);
            return;
        }
        let b_len = AuthButton::ALL.len();
        match key.code {
            KeyCode::Esc => {
                // Back to the hostname screen, keeping the typed hostname.
                self.status_message = None;
                self.screen = Screen::SetHostname {
                    input: auth.pending_hostname,
                    button: HostnameButton::Set,
                    focus: Focus::List,
                };
                return;
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.show_password = !self.show_password;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                auth.focus = if auth.focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Down | KeyCode::Up => {
                auth.focus = if auth.focus == Focus::List {
                    Focus::Buttons
                } else {
                    Focus::List
                };
            }
            KeyCode::Char('j') | KeyCode::Char('k') => {
                // In the password field these are plain characters —
                // passwords may contain anything.
                if auth.focus == Focus::Buttons {
                    auth.focus = Focus::List;
                } else if let Some(c) = text_char(&key) {
                    auth.password.push(c);
                }
            }
            KeyCode::Left => {
                if auth.focus == Focus::Buttons {
                    auth.button = AuthButton::ALL[(auth.button as usize + b_len - 1) % b_len];
                }
            }
            KeyCode::Char('h') => {
                if auth.focus == Focus::Buttons {
                    auth.button = AuthButton::ALL[(auth.button as usize + b_len - 1) % b_len];
                } else if let Some(c) = text_char(&key) {
                    auth.password.push(c);
                }
            }
            KeyCode::Right => {
                if auth.focus == Focus::Buttons {
                    auth.button = AuthButton::ALL[(auth.button as usize + 1) % b_len];
                }
            }
            KeyCode::Char('l') => {
                if auth.focus == Focus::Buttons {
                    auth.button = AuthButton::ALL[(auth.button as usize + 1) % b_len];
                } else if let Some(c) = text_char(&key) {
                    auth.password.push(c);
                }
            }
            KeyCode::Enter => {
                if auth.focus == Focus::List {
                    // Enter in the password field submits immediately.
                    let name = auth.pending_hostname.clone();
                    let pass = std::mem::take(&mut auth.password);
                    self.submit_hostname_auth(name, pass);
                    return;
                }
                match auth.button {
                    AuthButton::Cancel => {
                        self.status_message = None;
                        self.screen = Screen::SetHostname {
                            input: auth.pending_hostname,
                            button: HostnameButton::Set,
                            focus: Focus::List,
                        };
                        return;
                    }
                    AuthButton::Authenticate => {
                        let name = auth.pending_hostname.clone();
                        let pass = std::mem::take(&mut auth.password);
                        self.submit_hostname_auth(name, pass);
                        return;
                    }
                }
            }
            KeyCode::Backspace | KeyCode::Delete => {
                if auth.focus == Focus::List {
                    auth.password.pop();
                }
            }
            _ => {
                if let Some(c) = text_char(&key) {
                    if auth.focus == Focus::List {
                        auth.password.push(c);
                    }
                }
            }
        }
        self.screen = Screen::RootAuth(auth);
    }

    fn dialog_push(d: &mut AgentDialog, c: char) {
        match d {
            AgentDialog::Passphrase { pass, .. }
            | AgentDialog::PrivateKeyPassphrase { pass, .. } => pass.push(c),
            AgentDialog::UserPassword {
                user,
                pass,
                editing_user,
                ..
            }
            | AgentDialog::UserNameAndPassword {
                user,
                pass,
                editing_user,
                ..
            } => {
                if *editing_user {
                    user.push(c)
                } else {
                    pass.push(c)
                }
            }
        }
    }

    fn dialog_pop(d: &mut AgentDialog) {
        match d {
            AgentDialog::Passphrase { pass, .. }
            | AgentDialog::PrivateKeyPassphrase { pass, .. } => {
                pass.pop();
            }
            AgentDialog::UserPassword {
                user,
                pass,
                editing_user,
                ..
            }
            | AgentDialog::UserNameAndPassword {
                user,
                pass,
                editing_user,
                ..
            } => {
                if *editing_user {
                    user.pop();
                } else {
                    pass.pop();
                }
            }
        }
    }

    pub fn handle_agent_dialog_key(&mut self, mut dialog: AgentDialog, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                let prev = *dialog.take_prev_screen();
                dialog.cancel();
                self.status_message = Some("Cancelled".into());
                self.screen = prev;
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Toggle password visibility.
                self.show_password = !self.show_password;
                self.screen = Screen::AgentDialog(dialog);
            }
            KeyCode::Enter => match &mut dialog {
                AgentDialog::Passphrase { pass, .. }
                | AgentDialog::PrivateKeyPassphrase { pass, .. } => {
                    let owned_pass = std::mem::take(pass);
                    let prev = *dialog.take_prev_screen();
                    match dialog {
                        AgentDialog::Passphrase { reply_to, .. } => {
                            let _ = reply_to.send(AgentReply::Ok(owned_pass));
                        }
                        AgentDialog::PrivateKeyPassphrase { reply_to, .. } => {
                            let _ = reply_to.send(AgentReply::Ok(owned_pass));
                        }
                        _ => unreachable!(),
                    }
                    self.status_message = Some("Authenticating...".into());
                    self.screen = prev;
                }
                AgentDialog::UserPassword {
                    user,
                    pass,
                    editing_user,
                    ..
                }
                | AgentDialog::UserNameAndPassword {
                    user,
                    pass,
                    editing_user,
                    ..
                } => {
                    if *editing_user {
                        *editing_user = false;
                        self.screen = Screen::AgentDialog(dialog);
                    } else {
                        let owned_user = std::mem::take(user);
                        let owned_pass = std::mem::take(pass);
                        let prev = *dialog.take_prev_screen();
                        match dialog {
                            AgentDialog::UserPassword { reply_to, .. } => {
                                let _ = reply_to.send(AgentReply::Ok(owned_pass));
                            }
                            AgentDialog::UserNameAndPassword { reply_to, .. } => {
                                let _ = reply_to.send(AgentReply::Ok((owned_user, owned_pass)));
                            }
                            _ => unreachable!(),
                        }
                        self.status_message = Some("Authenticating...".into());
                        self.screen = prev;
                    }
                }
            },
            KeyCode::Tab | KeyCode::Down | KeyCode::Up => {
                if let AgentDialog::UserNameAndPassword { editing_user, .. } = &mut dialog {
                    if key.code == KeyCode::Up {
                        *editing_user = true;
                    } else {
                        *editing_user = !*editing_user;
                    }
                }
                self.screen = Screen::AgentDialog(dialog);
            }
            KeyCode::Backspace | KeyCode::Delete => {
                Self::dialog_pop(&mut dialog);
                self.screen = Screen::AgentDialog(dialog);
            }
            _ => {
                if let Some(c) = text_char(&key) {
                    Self::dialog_push(&mut dialog, c);
                }
                self.screen = Screen::AgentDialog(dialog);
            }
        }
    }
}

/// Returns the char for plain (unmodified / shift-only) key presses.
fn text_char(key: &KeyEvent) -> Option<char> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char(c) => Some(c),
        _ => None,
    }
}

// ── background actions ───────────────────────────────────────────
// Every action runs in its own task with a timeout and reports back through
// `AppEvent::ActionResult`, so the UI never blocks on iwd.

/// Spawn `fut` with a timeout; map the outcome into an `ActionResult` that
/// shows `success` in the status bar on success.
fn spawn_action(
    tx: mpsc::Sender<AppEvent>,
    success: String,
    timeout_secs: u64,
    fut: impl Future<Output = AppResult<()>> + Send + 'static,
) {
    tokio::spawn(async move {
        let result = match tokio::time::timeout(Duration::from_secs(timeout_secs), fut).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("timed out after {timeout_secs}s")),
        };
        let _ = tx.send(AppEvent::ActionResult { result, success }).await;
    });
}

impl App {
    /// Background scan. Sets a status message immediately; real failures
    /// (not "already in progress") surface in the status bar.
    pub fn spawn_scan(&mut self) {
        self.status_message = Some("Scanning...".into());
        let manager = self.iwd_manager.clone();
        let tx = self.app_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = manager.trigger_scan().await {
                let msg = e.to_string();
                if !is_scan_in_progress(&msg) {
                    let _ = tx
                        .send(AppEvent::FetchFailed(format!(
                            "Scan: {}",
                            human_error(&msg)
                        )))
                        .await;
                }
            }
        });
    }

    /// Connect to the highlighted network, or disconnect if it is the
    /// currently connected one (nmtui-style toggle).
    pub fn toggle_connection(&mut self, list_idx: usize) {
        let Some(net) = self.networks.get(list_idx).cloned() else {
            return;
        };
        let tx = self.app_tx.clone();
        let manager = self.iwd_manager.clone();
        if net.connected {
            self.status_message = Some(format!("Disconnecting from '{}'...", net.name));
            spawn_action(
                tx,
                format!("Disconnected from '{}'", net.name),
                10,
                async move { manager.disconnect().await },
            );
        } else {
            let path = net.path.clone();
            self.status_message = Some(format!("Connecting to '{}'...", net.name));
            spawn_action(tx, format!("Connected to '{}'", net.name), 60, async move {
                manager.connect_network(path).await
            });
        }
    }

    pub fn forget_network(&mut self, path: OwnedObjectPath, name: String) {
        self.status_message = Some(format!("Forgetting '{}'...", name));
        let manager = self.iwd_manager.clone();
        spawn_action(
            self.app_tx.clone(),
            format!("Forgot '{name}'"),
            10,
            async move { manager.forget_known_network(&path).await },
        );
    }

    pub fn save_auto_connect(&mut self, path: OwnedObjectPath, enabled: bool) {
        self.status_message = Some("Saving...".into());
        let manager = self.iwd_manager.clone();
        spawn_action(
            self.app_tx.clone(),
            if enabled {
                "Saved (auto-connect on)".into()
            } else {
                "Saved (auto-connect off)".into()
            },
            10,
            async move { manager.set_auto_connect(&path, enabled).await },
        );
    }

    pub fn connect_hidden(&mut self, ssid: String) {
        self.status_message = Some(format!("Connecting to hidden network '{ssid}'..."));
        let manager = self.iwd_manager.clone();
        spawn_action(
            self.app_tx.clone(),
            format!("Connected to '{ssid}'"),
            60,
            async move { manager.connect_hidden_network(&ssid).await },
        );
    }

    /// Toggle the Wi-Fi radio (Adapter.Powered) via the `p` key.
    pub fn toggle_wifi_power(&mut self) {
        let manager = self.iwd_manager.clone();
        let tx = self.app_tx.clone();
        tokio::spawn(async move {
            let result = match manager.is_wifi_powered().await {
                Ok(cur) => match manager.set_wifi_powered(!cur).await {
                    Ok(()) => {
                        // The Station disappears/reappears on power toggle;
                        // drop the cached path so the next call re-resolves.
                        manager.invalidate_station_path().await;
                        Ok(())
                    }
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            };
            let was_on = matches!(&result, Ok(()));
            let success = if was_on {
                "Wi-Fi disabled".to_string()
            } else {
                "Wi-Fi enabled".to_string()
            };
            // On failure report a plain error string (direction unknown).
            let _ = tx.send(AppEvent::ActionResult { result, success }).await;
        });
    }

    /// Apply the hostname entered on the Set-Hostname screen. Runs the
    /// privileged attempt directly (cheap syscall) and, when the process
    /// lacks permission, opens the root-password dialog instead of showing
    /// a "Permission denied" error — exactly like nmtui. Validation errors
    /// never reach this function as a modal: the hostname screen shows them
    /// inline while typing.
    pub fn submit_hostname(&mut self, name: String) {
        match system::set_hostname(&name) {
            Ok(()) => {
                self.hostname = name;
                self.status_message = Some("Hostname updated".into());
                self.screen = Screen::Main(2);
            }
            Err(system::HostnameSetError::AuthFailed) => {
                // Needs privileges: ask for the root password.
                self.status_message = Some("Root password required".into());
                self.screen = Screen::RootAuth(RootAuth {
                    pending_hostname: name,
                    password: String::new(),
                    message: None,
                    button: AuthButton::Authenticate,
                    focus: Focus::List,
                    busy: false,
                });
            }
            Err(e) => {
                let input = name.clone();
                self.error_prev_screen = Some(Box::new(Screen::SetHostname {
                    input,
                    button: HostnameButton::Set,
                    focus: Focus::List,
                }));
                self.screen = Screen::Error(human_error(&e.to_string()));
            }
        }
    }

    /// The "Authenticate" path of the root-password dialog: run the
    /// hostname set through `sudo -S` in the background (with a timeout)
    /// and report back via `AppEvent::HostnameSet`. The dialog stays on
    /// screen in a busy state until the result arrives.
    pub fn submit_hostname_auth(&mut self, name: String, password: String) {
        self.status_message = Some("Authenticating...".into());
        if let Screen::RootAuth(auth) = &mut self.screen {
            if auth.pending_hostname == name {
                auth.busy = true;
                auth.message = None;
            }
        }
        let tx = self.app_tx.clone();
        let name_for_event = name.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(20),
                tokio::task::spawn_blocking(move || {
                    system::set_hostname_elevated(&name, &password)
                }),
            )
            .await
            {
                Ok(Ok(inner)) => inner,
                Ok(Err(join_err)) => Err(system::HostnameSetError::Other(format!(
                    "The privileged task failed to run: {join_err}"
                ))),
                Err(_) => Err(system::HostnameSetError::Other(
                    "sudo timed out after 20s".into(),
                )),
            };
            let _ = tx
                .send(AppEvent::HostnameSet {
                    name: name_for_event,
                    result,
                })
                .await;
        });
    }

    /// Re-fetch both lists after a successful (or cancelled) action so the
    /// UI reflects reality instantly instead of waiting for the next
    /// periodic refresh.
    pub fn refresh_after_action(&mut self) {
        self.refresh_data(true, true);
    }

    /// Spawn non-blocking fetches; failures surface in the status bar.
    pub fn refresh_data(&mut self, networks: bool, known: bool) {
        if self.iwd_manager.conn.is_none() {
            return;
        }
        self.last_refresh = Instant::now();

        if networks {
            let manager = self.iwd_manager.clone();
            let tx = self.app_tx.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(Duration::from_secs(10), manager.get_networks()).await {
                    Ok(Ok(nets)) => {
                        let _ = tx.send(AppEvent::NetworksUpdated(nets)).await;
                    }
                    Ok(Err(e)) => {
                        let _ = tx
                            .send(AppEvent::FetchFailed(human_error(&e.to_string())))
                            .await;
                    }
                    Err(_) => {
                        let _ = tx
                            .send(AppEvent::FetchFailed("iwd took too long to answer".into()))
                            .await;
                    }
                }
            });

            // Station state piggybacks on network refreshes.
            let manager = self.iwd_manager.clone();
            let tx = self.app_tx.clone();
            tokio::spawn(async move {
                if let Ok(Ok(state)) =
                    tokio::time::timeout(Duration::from_secs(5), manager.get_station_state()).await
                {
                    let _ = tx.send(AppEvent::StationStateUpdated(state)).await;
                }
            });
        }

        if known {
            let manager = self.iwd_manager.clone();
            let tx = self.app_tx.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(Duration::from_secs(10), manager.get_known_networks())
                    .await
                {
                    Ok(Ok(known)) => {
                        let _ = tx.send(AppEvent::KnownNetworksUpdated(known)).await;
                    }
                    Ok(Err(e)) => {
                        let _ = tx
                            .send(AppEvent::FetchFailed(human_error(&e.to_string())))
                            .await;
                    }
                    Err(_) => {
                        let _ = tx
                            .send(AppEvent::FetchFailed("iwd took too long to answer".into()))
                            .await;
                    }
                }
            });
        }
    }
}

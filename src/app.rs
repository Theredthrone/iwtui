//! Application state and key handling.
//!
//! The UI is a stack of windows:
//!
//! * one base screen (`Screen::Menu` or `Screen::Wifi`), plus
//! * `App::overlays` - every push goes one level deeper, Esc pops back.
//!
//! Rendering lives in `ui.rs`, all D-Bus work in `iwd.rs`.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::{mpsc, oneshot};
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

use crate::iwd::{self, NetworkEntry, Snapshot};

/// Events from the background tasks to the main loop.
pub enum AppEvent {
    Input(Event),
    /// Something in iwd changed; reload the snapshot (debounced).
    IwdChanged,
    /// iwd is asking us for credentials.
    AgentQuery(AgentQuery),
    /// iwd no longer wants the answer to a pending agent query.
    AgentCancelled(String),
    /// A background command failed.
    Error(String),
}

pub enum AgentQuery {
    Passphrase {
        network: OwnedObjectPath,
        respond: oneshot::Sender<Option<String>>,
    },
}

pub const MENU_ENTRIES: [&str; 2] = ["Wi-Fi networks", "Quit"];
/// Buttons in the right-hand column of the Wi-Fi screen (nmtui style).
pub const WIFI_BUTTONS: [&str; 4] = ["Connect", "Disconnect", "Rescan", "Back"];
/// Buttons in the network details window (2-column grid).
pub const DETAIL_BUTTONS: [&str; 4] = ["Connect", "Disconnect", "Forget", "Back"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    NetworkList,
    Buttons,
}

pub enum Screen {
    Menu { selected: usize },
    Wifi {
        focus: Focus,
        /// Selected row in the network list.
        selected: usize,
        /// Selected button in the right-hand column.
        button: usize,
    },
}

/// One layer of the window stack.
pub enum Overlay {
    NetworkDetails {
        entry: NetworkEntry,
        /// Selected button in the 2x2 grid.
        button: usize,
    },
    Passphrase {
        network_name: String,
        input: String,
        respond: oneshot::Sender<Option<String>>,
    },
    ConfirmForget {
        entry: NetworkEntry,
    },
    Message {
        title: String,
        text: String,
    },
}

pub struct App {
    pub conn: Connection,
    pub events: mpsc::Sender<AppEvent>,
    pub wifi: Option<Snapshot>,
    pub screen: Screen,
    pub overlays: Vec<Overlay>,
    pub dirty: bool,
    pub quit: bool,
    pub last_error: Option<String>,
}

impl App {
    pub fn new(conn: Connection, events: mpsc::Sender<AppEvent>) -> Self {
        App {
            conn,
            events,
            wifi: None,
            screen: Screen::Menu { selected: 0 },
            overlays: Vec::new(),
            dirty: false,
            quit: false,
            last_error: None,
        }
    }

    pub fn on_wifi_screen(&self) -> bool {
        matches!(self.screen, Screen::Wifi { .. })
    }

    pub fn push_error(&mut self, text: String) {
        self.overlays.push(Overlay::Message { title: "Error".to_owned(), text });
    }

    // ------------------------------------------------------------ input

    pub async fn handle_input(&mut self, event: Event) {
        let Event::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.overlays.is_empty() {
            self.handle_screen_key(key.code).await;
        } else {
            self.handle_overlay_key(key.code).await;
        }
    }

    async fn handle_screen_key(&mut self, code: KeyCode) {
        if matches!(self.screen, Screen::Menu { .. }) {
            self.handle_menu_key(code).await;
        } else {
            self.handle_wifi_key(code).await;
        }
    }

    async fn handle_menu_key(&mut self, code: KeyCode) {
        let selected = match &self.screen {
            Screen::Menu { selected } => *selected,
            _ => return,
        };
        match code {
            KeyCode::Up => {
                self.set_menu_selected((selected + MENU_ENTRIES.len() - 1) % MENU_ENTRIES.len())
            }
            KeyCode::Down => self.set_menu_selected((selected + 1) % MENU_ENTRIES.len()),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if selected == 0 {
                    self.enter_wifi().await;
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    async fn enter_wifi(&mut self) {
        self.screen = Screen::Wifi { focus: Focus::NetworkList, selected: 0, button: 0 };
        self.reload().await;
        self.spawn_scan();
    }

    async fn handle_wifi_key(&mut self, code: KeyCode) {
        let (focus, selected, button) = match &self.screen {
            Screen::Wifi { focus, selected, button } => (*focus, *selected, *button),
            _ => return,
        };
        let network_count = self.wifi.as_ref().map(|w| w.networks.len()).unwrap_or(0);

        match code {
            KeyCode::Esc => self.leave_wifi(),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Char('r') => self.spawn_scan(),
            _ => match focus {
                Focus::NetworkList => match code {
                    KeyCode::Up => self.set_wifi_selected(selected.saturating_sub(1)),
                    KeyCode::Down => {
                        if selected + 1 < network_count {
                            self.set_wifi_selected(selected + 1);
                        }
                    }
                    KeyCode::Right => self.set_focus(Focus::Buttons),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if let Some(entry) = self.network_at(selected) {
                            self.overlays.push(Overlay::NetworkDetails { entry, button: 0 });
                        }
                    }
                    _ => {}
                },
                Focus::Buttons => match code {
                    KeyCode::Up => {
                        self.set_wifi_button((button + WIFI_BUTTONS.len() - 1) % WIFI_BUTTONS.len())
                    }
                    KeyCode::Down => self.set_wifi_button((button + 1) % WIFI_BUTTONS.len()),
                    KeyCode::Left => self.set_focus(Focus::NetworkList),
                    KeyCode::Enter | KeyCode::Char(' ') => match button {
                        0 => self.spawn_connect_selected(),
                        1 => self.spawn_disconnect(),
                        2 => self.spawn_scan(),
                        _ => self.leave_wifi(),
                    },
                    _ => {}
                },
            },
        }
    }

    fn leave_wifi(&mut self) {
        self.screen = Screen::Menu { selected: 0 };
    }

    async fn handle_overlay_key(&mut self, code: KeyCode) {
        // Copy the data needed to decide first, so the arms below can
        // freely push/pop overlays and spawn commands.
        enum Top {
            Details(NetworkEntry, usize),
            Passphrase,
            ConfirmForget(NetworkEntry),
            Message,
        }
        let top = match self.overlays.last() {
            Some(Overlay::NetworkDetails { entry, button }) => {
                Top::Details(entry.clone(), *button)
            }
            Some(Overlay::Passphrase { .. }) => Top::Passphrase,
            Some(Overlay::ConfirmForget { entry }) => Top::ConfirmForget(entry.clone()),
            Some(Overlay::Message { .. }) => Top::Message,
            None => return,
        };

        match top {
            Top::Message => {
                if matches!(
                    code,
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') | KeyCode::Char('q')
                ) {
                    self.overlays.pop();
                }
            }
            Top::Passphrase => match code {
                KeyCode::Esc => self.answer_passphrase(None),
                KeyCode::Enter => {
                    let input = match self.overlays.last() {
                        Some(Overlay::Passphrase { input, .. }) => input.clone(),
                        _ => String::new(),
                    };
                    self.answer_passphrase(Some(input));
                }
                KeyCode::Backspace => {
                    if let Some(Overlay::Passphrase { input, .. }) = self.overlays.last_mut() {
                        input.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(Overlay::Passphrase { input, .. }) = self.overlays.last_mut() {
                        input.push(c);
                    }
                }
                _ => {}
            },
            Top::Details(entry, button) => match code {
                // 2x2 grid: vertical travel is +/-2, horizontal +/-1.
                KeyCode::Up | KeyCode::Down => {
                    self.set_details_button((button + 2) % DETAIL_BUTTONS.len())
                }
                KeyCode::Right | KeyCode::Tab => {
                    self.set_details_button((button + 1) % DETAIL_BUTTONS.len())
                }
                KeyCode::Left => {
                    self.set_details_button((button + DETAIL_BUTTONS.len() - 1) % DETAIL_BUTTONS.len())
                }
                KeyCode::Esc => {
                    self.overlays.pop();
                }
                KeyCode::Enter | KeyCode::Char(' ') => match button {
                    0 => {
                        self.overlays.pop();
                        self.spawn_connect(&entry.path);
                    }
                    1 => {
                        self.overlays.pop();
                        self.spawn_disconnect();
                    }
                    2 => {
                        // one window deeper: details -> confirmation
                        self.overlays.push(Overlay::ConfirmForget { entry });
                    }
                    _ => {
                        self.overlays.pop();
                    }
                },
                _ => {}
            },
            Top::ConfirmForget(entry) => match code {
                KeyCode::Enter | KeyCode::Char(' ') => {
                    // pop the confirmation and the details window under it
                    self.overlays.pop();
                    self.overlays.pop();
                    self.spawn_forget(&entry.path);
                }
                KeyCode::Esc => {
                    self.overlays.pop();
                }
                _ => {}
            },
        }
    }

    fn answer_passphrase(&mut self, answer: Option<String>) {
        if let Some(Overlay::Passphrase { respond, .. }) = self.overlays.pop() {
            let _ = respond.send(answer);
        }
    }

    // ------------------------------------------------------------ agent

    pub fn handle_agent_query(&mut self, query: AgentQuery) {
        match query {
            AgentQuery::Passphrase { network, respond } => {
                let network_name = self
                    .wifi
                    .as_ref()
                    .and_then(|w| w.networks.iter().find(|n| n.path == network))
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| "network".to_owned());
                self.overlays.push(Overlay::Passphrase {
                    network_name,
                    input: String::new(),
                    respond,
                });
            }
        }
    }

    pub fn handle_agent_cancel(&mut self, _reason: String) {
        if matches!(self.overlays.last(), Some(Overlay::Passphrase { .. })) {
            self.answer_passphrase(None);
        }
    }

    // ------------------------------------------------------------ state

    /// Re-read everything from iwd; keeps the list selection stable by
    /// tracking the previously selected network path.
    pub async fn reload(&mut self) {
        let previous = match &self.screen {
            Screen::Wifi { selected, .. } => self
                .wifi
                .as_ref()
                .and_then(|w| w.networks.get(*selected))
                .map(|n| n.path.clone()),
            _ => None,
        };

        match iwd::snapshot(&self.conn).await {
            Ok(snapshot) => {
                if let (Some(w), Some(path)) = (&snapshot, previous) {
                    if let Screen::Wifi { selected, .. } = &mut self.screen {
                        *selected = w.networks.iter().position(|n| n.path == path).unwrap_or(0);
                    }
                }
                self.wifi = snapshot;
                self.last_error = None;
            }
            Err(e) => {
                self.wifi = None;
                self.last_error = Some(format!("iwd unreachable: {e}"));
            }
        }
    }

    // ---------------------------------------------------------- commands
    // All commands are spawned: none may block the UI, and Connect()
    // can legitimately wait on the passphrase window.

    fn spawn_scan(&self) {
        if let Some(w) = &self.wifi {
            let conn = self.conn.clone();
            let station = w.station.clone();
            let events = self.events.clone();
            tokio::spawn(async move {
                if let Err(e) = iwd::scan(&conn, &station).await {
                    let _ = events.send(AppEvent::Error(format!("scan: {e}"))).await;
                }
            });
        }
    }

    fn spawn_connect(&self, network: &OwnedObjectPath) {
        let conn = self.conn.clone();
        let path = network.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            if let Err(e) = iwd::connect(&conn, &path).await {
                let _ = events.send(AppEvent::Error(format!("connect: {e}"))).await;
            }
        });
    }

    fn spawn_connect_selected(&mut self) {
        let selected = match &self.screen {
            Screen::Wifi { selected, .. } => *selected,
            _ => return,
        };
        if let Some(entry) = self.network_at(selected) {
            self.spawn_connect(&entry.path);
        }
    }

    fn spawn_disconnect(&self) {
        if let Some(w) = &self.wifi {
            let conn = self.conn.clone();
            let station = w.station.clone();
            let events = self.events.clone();
            tokio::spawn(async move {
                if let Err(e) = iwd::disconnect(&conn, &station).await {
                    let _ = events.send(AppEvent::Error(format!("disconnect: {e}"))).await;
                }
            });
        }
    }

    fn spawn_forget(&self, network: &OwnedObjectPath) {
        let conn = self.conn.clone();
        let path = network.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            if let Err(e) = iwd::forget(&conn, &path).await {
                let _ = events.send(AppEvent::Error(format!("forget: {e}"))).await;
            }
        });
    }

    // ---------------------------------------------------------- helpers

    fn network_at(&self, index: usize) -> Option<NetworkEntry> {
        self.wifi.as_ref()?.networks.get(index).cloned()
    }

    fn set_menu_selected(&mut self, n: usize) {
        if let Screen::Menu { selected } = &mut self.screen {
            *selected = n;
        }
    }

    fn set_wifi_selected(&mut self, n: usize) {
        if let Screen::Wifi { selected, .. } = &mut self.screen {
            *selected = n;
        }
    }

    fn set_wifi_button(&mut self, n: usize) {
        if let Screen::Wifi { button, .. } = &mut self.screen {
            *button = n;
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        if let Screen::Wifi { focus: f, .. } = &mut self.screen {
            *f = focus;
        }
    }

    fn toggle_focus(&mut self) {
        let next = match &self.screen {
            Screen::Wifi { focus, .. } => match focus {
                Focus::NetworkList => Focus::Buttons,
                Focus::Buttons => Focus::NetworkList,
            },
            _ => return,
        };
        self.set_focus(next);
    }

    fn set_details_button(&mut self, n: usize) {
        if let Some(Overlay::NetworkDetails { button, .. }) = self.overlays.last_mut() {
            *button = n;
        }
    }
}

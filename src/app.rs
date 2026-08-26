use std::sync::Arc;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use tokio::sync::{mpsc, Notify};
use zbus::Connection;

use crate::agent::{AgentReply, AgentRequest, IwdAgent};
use crate::iwd::{AppKnownNetwork, AppNetwork, IwdEvent, IwdManager};
use crate::system;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    List,
    Buttons,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActivateButton {
    Rescan,
    Disconnect,
    Quit,
}

impl ActivateButton {
    pub const ALL: [Self; 3] = [Self::Rescan, Self::Disconnect, Self::Quit];
    pub fn label(self) -> &'static str {
        match self {
            Self::Rescan => "Rescan",
            Self::Disconnect => "Disconnect",
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

pub enum AppEvent {
    ActionResult(Result<(), String>),
    NetworksUpdated(Vec<AppNetwork>),
    KnownNetworksUpdated(Vec<AppKnownNetwork>),
}

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
            AgentDialog::Passphrase { network_name, .. } => {
                format!("Passwords are required to connect to '{network_name}'")
            }
            AgentDialog::UserPassword { network_name, user, .. } => {
                format!("Passwords are required to connect to '{network_name}'\nUsername: {user}")
            }
            AgentDialog::UserNameAndPassword { network_name, .. } => {
                format!("Credentials are required to connect to '{network_name}'")
            }
            AgentDialog::PrivateKeyPassphrase { network_name, .. } => {
                format!("Private key passphrase required for '{network_name}'")
            }
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
            AgentDialog::Passphrase { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
            AgentDialog::UserPassword { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
            AgentDialog::PrivateKeyPassphrase { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
            AgentDialog::UserNameAndPassword { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
        }
    }
}

pub struct EditForm {
    pub net_idx: usize,
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
    AgentDialog(AgentDialog),
    Error(String),
}

pub struct App {
    pub screen: Screen,
    pub iwd_manager: IwdManager,
    pub networks: Vec<AppNetwork>,
    pub known_networks: Vec<AppKnownNetwork>,
    pub hostname: String,
    pub device_name: String,
    pub should_quit: bool,
    pub agent_rx: mpsc::Receiver<AgentRequest>,
    pub iwd_events: mpsc::Receiver<IwdEvent>,
    pub app_rx: mpsc::Receiver<AppEvent>,
    pub app_tx: mpsc::Sender<AppEvent>,
    pub status_message: Option<String>,
    pub cancel_notify: Arc<Notify>,
    pub error_prev_screen: Option<Box<Screen>>,
}

impl App {
    pub async fn new() -> App {
        let mut init_error = None;
        let conn = match Connection::system().await {
            Ok(c) => Some(c),
            Err(e) => {
                init_error = Some(format!("Failed to connect to system D-Bus: {e}"));
                None
            }
        };

        let mut iwd_manager = IwdManager::new(conn.clone());
        if conn.is_some() {
            if let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(2), iwd_manager.init_station_path()).await.unwrap_or(Err(crate::err("timeout"))) {
                eprintln!("Warning: could not initialize IWD station: {e}");
            }
        }

        let (agent_tx, agent_rx) = mpsc::channel(10);
        let cancel_notify = Arc::new(Notify::new());

        let (app_tx, app_rx) = mpsc::channel(10);

        if let Some(conn) = conn.clone() {
            let agent = IwdAgent { tx: agent_tx, conn: conn.clone(), cancel: cancel_notify.clone() };
            if let Err(e) = conn.object_server().at("/net/connman/iwd/agent", agent).await {
                init_error = Some(format!("Failed to register agent object: {e}"));
            } else {
                if let Ok(agent_manager) = crate::iwd::AgentManagerProxy::new(&conn).await {
                    let agent_path = zbus::zvariant::OwnedObjectPath::try_from("/net/connman/iwd/agent").unwrap();
                    if let Err(e) = agent_manager.register_agent(&agent_path).await {
                        init_error = Some(format!("Failed to register agent with IWD: {e}"));
                    }
                } else {
                    init_error = Some("IWD not running? AgentManager lookup failed".to_string());
                }
            }
        }

        let mut iwd_events = if conn.is_some() {
            iwd_manager.spawn_signal_listener()
        } else {
            mpsc::channel(10).1
        };

        let networks = tokio::time::timeout(std::time::Duration::from_secs(2), iwd_manager.get_networks()).await.unwrap_or_else(|_| Ok(Vec::new())).unwrap_or_default();
        let known_networks = tokio::time::timeout(std::time::Duration::from_secs(2), iwd_manager.get_known_networks()).await.unwrap_or_else(|_| Ok(Vec::new())).unwrap_or_default();
        let hostname = system::get_hostname().unwrap_or_else(|_| "localhost".to_string());
        let device_name = tokio::time::timeout(std::time::Duration::from_secs(2), iwd_manager.get_device_name()).await.unwrap_or_else(|_| None).unwrap_or_else(|| "wlan0".to_string());

        while iwd_events.try_recv().is_ok() {}

        let mut screen = Screen::Main(0);
        if let Some(err) = init_error.take() {
            screen = Screen::Error(err);
        }

        App {
            screen,
            iwd_manager,
            networks,
            known_networks,
            hostname,
            device_name,
            should_quit: false,
            agent_rx,
            iwd_events,
            app_rx,
            app_tx,
            status_message: None,
            cancel_notify,
            error_prev_screen: None,
        }
    }

    pub async fn shutdown(&self) {
        if let Some(conn) = &self.iwd_manager.conn {
            if let Ok(agent_manager) = crate::iwd::AgentManagerProxy::new(conn).await {
                let agent_path = zbus::zvariant::OwnedObjectPath::try_from("/net/connman/iwd/agent").unwrap();
                let _ = agent_manager.unregister_agent(&agent_path).await;
            }
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
            if matches!(self.screen, Screen::AgentDialog(_)) {
                match req {
                    AgentRequest::RequestPassphrase { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
                    AgentRequest::RequestUserPassword { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
                    AgentRequest::RequestPrivateKeyPassphrase { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
                    AgentRequest::RequestUserNameAndPassword { reply_to, .. } => { let _ = reply_to.send(AgentReply::Cancelled); }
                }
                continue;
            }
            let prev = Box::new(std::mem::replace(&mut self.screen, Screen::Main(0)));
            match req {
                AgentRequest::RequestPassphrase { network_name, reply_to, .. } => {
                    self.screen = Screen::AgentDialog(AgentDialog::Passphrase {
                        network_name,
                        pass: String::new(),
                        reply_to,
                        prev_screen: prev,
                    });
                }
                AgentRequest::RequestUserPassword { network_name, user, reply_to, .. } => {
                    self.screen = Screen::AgentDialog(AgentDialog::UserPassword {
                        network_name,
                        user,
                        pass: String::new(),
                        editing_user: false,
                        reply_to,
                        prev_screen: prev,
                    });
                }
                AgentRequest::RequestUserNameAndPassword { network_name, reply_to, .. } => {
                    self.screen = Screen::AgentDialog(AgentDialog::UserNameAndPassword {
                        network_name,
                        user: String::new(),
                        pass: String::new(),
                        editing_user: true,
                        reply_to,
                        prev_screen: prev,
                    });
                }
                AgentRequest::RequestPrivateKeyPassphrase { network_name, reply_to, .. } => {
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

    pub fn poll_app_events(&mut self) {
        while let Ok(ev) = self.app_rx.try_recv() {
            match ev {
                AppEvent::ActionResult(Ok(())) => self.status_message = Some("Action completed successfully".into()),
                AppEvent::ActionResult(Err(e)) => {
                    self.error_prev_screen = Some(Box::new(std::mem::replace(&mut self.screen, Screen::Main(0))));
                    self.screen = Screen::Error(e);
                }
                AppEvent::NetworksUpdated(nets) => self.networks = nets,
                AppEvent::KnownNetworksUpdated(known) => self.known_networks = known,
            }
        }
    }

    pub fn poll_iwd_events(&mut self) {
        let mut need_networks = false;
        let mut need_known = false;
        while let Ok(ev) = self.iwd_events.try_recv() {
            match ev {
                IwdEvent::NetworksChanged | IwdEvent::ConnectedNetworkChanged => {
                    need_networks = true;
                }
                IwdEvent::KnownNetworksChanged => {
                    need_known = true;
                    need_networks = true;
                }
            }
        }
        if need_networks {
            let manager = self.iwd_manager.clone();
            let tx = self.app_tx.clone();
            tokio::spawn(async move {
                if let Ok(Ok(nets)) = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    manager.get_networks(),
                ).await {
                    let _ = tx.send(AppEvent::NetworksUpdated(nets)).await;
                }
            });
        }
        if need_known {
            let manager = self.iwd_manager.clone();
            let tx = self.app_tx.clone();
            tokio::spawn(async move {
                if let Ok(Ok(known)) = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    manager.get_known_networks(),
                ).await {
                    let _ = tx.send(AppEvent::KnownNetworksUpdated(known)).await;
                }
            });
        }
    }

    pub async fn handle_event(&mut self, ev: Event) -> bool {
        if let Event::Key(key) = ev {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return true;
            }

            let current = std::mem::replace(&mut self.screen, Screen::Main(0));
            match current {
                Screen::AgentDialog(dialog) => {
                    let (new_dialog, prev_screen) = Self::handle_agent_dialog_key(dialog, key);
                    match new_dialog {
                        Some(d) => self.screen = Screen::AgentDialog(d),
                        None => self.screen = prev_screen.unwrap_or(Screen::Main(0)),
                    }
                    return self.should_quit;
                }
                Screen::Main(idx) => self.handle_main_key(key, idx),
                Screen::Activate { list_idx, button, focus } => {
                    self.handle_activate_key(key, list_idx, button, focus).await
                }
                Screen::EditList { list_idx, button, focus } => {
                    self.handle_edit_list_key(key, list_idx, button, focus).await
                }
                Screen::EditForm(form) => self.handle_edit_form_key(key, form).await,
                Screen::AddHidden(form) => self.handle_add_hidden_key(key, form).await,
                Screen::SetHostname { input, button, focus } => {
                    self.handle_hostname_key(key, input, button, focus)
                }
                Screen::Error(msg) => {
                    if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                        self.screen = *self.error_prev_screen.take().unwrap_or(Box::new(Screen::Main(0)));
                    } else {
                        self.screen = Screen::Error(msg);
                    }
                }
            }
        }
        self.should_quit
    }

    fn handle_main_key(&mut self, key: crossterm::event::KeyEvent, idx: usize) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.screen = Screen::Main((idx + 1) % 4),
            KeyCode::Up | KeyCode::Char('k') => self.screen = Screen::Main((idx + 3) % 4),
            KeyCode::Enter => match idx {
                0 => self.screen = Screen::EditList {
                    list_idx: 0,
                    button: EditListButton::Quit,
                    focus: Focus::List,
                },
                1 => {
                    self.screen = Screen::Activate {
                        list_idx: 0,
                        button: ActivateButton::Quit,
                        focus: Focus::List,
                    };
                    if self.networks.is_empty() {
                        self.status_message = Some("Scanning...".into());
                        let tx = self.app_tx.clone();
                        let manager = self.iwd_manager.clone();
                        tokio::spawn(async move {
                            let _ = tx.send(AppEvent::ActionResult(manager.trigger_scan().await.map_err(|e| e.to_string()))).await;
                        });
                    }
                }
                2 => self.screen = Screen::SetHostname {
                    input: self.hostname.clone(),
                    button: HostnameButton::Set,
                    focus: Focus::List,
                },
                3 => self.should_quit = true,
                _ => {}
            },
            _ => {}
        }
    }

    async fn handle_activate_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        list_idx: usize,
        button: ActivateButton,
        focus: Focus,
    ) {
        let n_len = self.networks.len();
        let b_len = ActivateButton::ALL.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Main(1),
            KeyCode::Tab | KeyCode::BackTab => {
                let new_focus = if focus == Focus::List { Focus::Buttons } else { Focus::List };
                self.screen = Screen::Activate { list_idx, button, focus: new_focus };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if focus == Focus::List {
                    let new_idx = if n_len > 0 { (list_idx + 1) % n_len } else { 0 };
                    self.screen = Screen::Activate { list_idx: new_idx, button, focus };
                } else {
                    self.screen = Screen::Activate { list_idx, button, focus: Focus::List };
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if focus == Focus::List {
                    let new_idx = if n_len > 0 { (list_idx + n_len - 1) % n_len } else { 0 };
                    self.screen = Screen::Activate { list_idx: new_idx, button, focus };
                } else {
                    self.screen = Screen::Activate { list_idx, button, focus: Focus::List };
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if focus == Focus::Buttons {
                    let next = button as usize;
                    let new_b = ActivateButton::ALL[(next + b_len - 1) % b_len];
                    self.screen = Screen::Activate { list_idx, button: new_b, focus };
                } else {
                    let new_b = ActivateButton::ALL[b_len - 1];
                    self.screen = Screen::Activate { list_idx, button: new_b, focus: Focus::Buttons };
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if focus == Focus::Buttons {
                    let next = button as usize;
                    let new_b = ActivateButton::ALL[(next + 1) % b_len];
                    self.screen = Screen::Activate { list_idx, button: new_b, focus };
                } else {
                    let new_b = ActivateButton::ALL[0];
                    self.screen = Screen::Activate { list_idx, button: new_b, focus: Focus::Buttons };
                }
            }
            KeyCode::Enter => match focus {
                Focus::List => {
                    if let Some(net) = self.networks.get(list_idx).cloned() {
                        if net.connected {
                            self.status_message = Some("Already connected".into());
                            self.screen = Screen::Activate { list_idx, button, focus };
                        } else {
                            let tx = self.app_tx.clone();
                            let manager = self.iwd_manager.clone();
                            let path = net.path.clone();
                            tokio::spawn(async move {
                                let res = manager.connect_network(path).await;
                                let _ = tx.send(AppEvent::ActionResult(res.map_err(|e| e.to_string()))).await;
                            });
                            self.status_message = Some("Connecting...".into());
                            self.screen = Screen::Activate { list_idx, button, focus };
                        }
                    } else {
                        self.screen = Screen::Activate { list_idx, button, focus };
                    }
                }
                Focus::Buttons => match button {
                    ActivateButton::Rescan => {
                        let manager = self.iwd_manager.clone();
                        let tx = self.app_tx.clone();
                        tokio::spawn(async move {
                            let res = tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                manager.trigger_scan(),
                            ).await;
                            let result = match res {
                                Ok(Ok(())) => Ok(()),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(_) => Err("Timeout".to_string()),
                            };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                        self.status_message = Some("Scanning...".into());
                        self.screen = Screen::Activate { list_idx, button, focus };
                    }
                    ActivateButton::Disconnect => {
                        let manager = self.iwd_manager.clone();
                        let tx = self.app_tx.clone();
                        tokio::spawn(async move {
                            let res = tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                manager.disconnect(),
                            ).await;
                            let result = match res {
                                Ok(Ok(())) => Ok(()),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(_) => Err("Timeout".to_string()),
                            };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                        self.status_message = Some("Disconnecting...".into());
                        self.screen = Screen::Activate { list_idx, button, focus };
                    }
                    ActivateButton::Quit => self.screen = Screen::Main(1),
                },
            },
            _ => self.screen = Screen::Activate { list_idx, button, focus },
        }
    }

    async fn handle_edit_list_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        list_idx: usize,
        button: EditListButton,
        focus: Focus,
    ) {
        let n_len = self.known_networks.len();
        let b_len = EditListButton::ALL.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Main(0),
            KeyCode::Tab | KeyCode::BackTab => {
                let new_focus = if focus == Focus::List { Focus::Buttons } else { Focus::List };
                self.screen = Screen::EditList { list_idx, button, focus: new_focus };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if focus == Focus::List {
                    let new_idx = if n_len > 0 { (list_idx + 1) % n_len } else { 0 };
                    self.screen = Screen::EditList { list_idx: new_idx, button, focus };
                } else {
                    self.screen = Screen::EditList { list_idx, button, focus: Focus::List };
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if focus == Focus::List {
                    let new_idx = if n_len > 0 { (list_idx + n_len - 1) % n_len } else { 0 };
                    self.screen = Screen::EditList { list_idx: new_idx, button, focus };
                } else {
                    self.screen = Screen::EditList { list_idx, button, focus: Focus::List };
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if focus == Focus::Buttons {
                    let next = button as usize;
                    let new_b = EditListButton::ALL[(next + b_len - 1) % b_len];
                    self.screen = Screen::EditList { list_idx, button: new_b, focus };
                } else {
                    let new_b = EditListButton::ALL[b_len - 1];
                    self.screen = Screen::EditList { list_idx, button: new_b, focus: Focus::Buttons };
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if focus == Focus::Buttons {
                    let next = button as usize;
                    let new_b = EditListButton::ALL[(next + 1) % b_len];
                    self.screen = Screen::EditList { list_idx, button: new_b, focus };
                } else {
                    let new_b = EditListButton::ALL[0];
                    self.screen = Screen::EditList { list_idx, button: new_b, focus: Focus::Buttons };
                }
            }
            KeyCode::Enter => match focus {
                Focus::List => {
                    if list_idx < n_len {
                        let net = self.known_networks[list_idx].clone();
                        self.screen = Screen::EditForm(EditForm {
                            net_idx: list_idx,
                            auto_connect: net.auto_connect,
                            button: EditFormButton::Save,
                            focus: Focus::List,
                        });
                    } else {
                        self.screen = Screen::EditList { list_idx, button, focus };
                    }
                }
                Focus::Buttons => match button {
                    EditListButton::Add => {
                        self.screen = Screen::AddHidden(AddHiddenForm {
                            ssid: String::new(),
                            button: AddHiddenButton::Connect,
                            focus: Focus::List,
                        });
                    }
                    EditListButton::Delete => {
                        if list_idx < n_len {
                            let net = self.known_networks[list_idx].clone();
                            let manager = self.iwd_manager.clone();
                            let tx = self.app_tx.clone();
                            tokio::spawn(async move {
                                let res = tokio::time::timeout(
                                    std::time::Duration::from_secs(2),
                                    manager.forget_known_network(&net.path),
                                ).await;
                                let result = match res {
                                    Ok(Ok(())) => Ok(()),
                                    Ok(Err(e)) => Err(e.to_string()),
                                    Err(_) => Err("Timeout".to_string()),
                                };
                                let _ = tx.send(AppEvent::ActionResult(result)).await;
                            });
                            self.status_message = Some(format!("Forgetting '{}'", net.name));
                            self.screen = Screen::EditList { list_idx, button, focus };
                        } else {
                            self.screen = Screen::EditList { list_idx, button, focus };
                        }
                    }
                    EditListButton::Quit => self.screen = Screen::Main(0),
                },
            },
            _ => self.screen = Screen::EditList { list_idx, button, focus },
        }
    }

    async fn handle_edit_form_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        mut form: EditForm,
    ) {
        let b_len = EditFormButton::ALL.len();
        match key.code {
            KeyCode::Esc => self.screen = Screen::EditList {
                list_idx: form.net_idx,
                button: EditListButton::Quit,
                focus: Focus::List,
            },
            KeyCode::Tab | KeyCode::BackTab => {
                form.focus = if form.focus == Focus::List { Focus::Buttons } else { Focus::List };
                self.screen = Screen::EditForm(form);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if form.focus == Focus::List {
                    form.focus = Focus::Buttons;
                } else {
                    form.focus = Focus::List;
                }
                self.screen = Screen::EditForm(form);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if form.focus == Focus::List {
                    form.focus = Focus::Buttons;
                } else {
                    form.focus = Focus::List;
                }
                self.screen = Screen::EditForm(form);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if form.focus == Focus::Buttons {
                    let next = form.button as usize;
                    form.button = EditFormButton::ALL[(next + b_len - 1) % b_len];
                }
                self.screen = Screen::EditForm(form);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if form.focus == Focus::Buttons {
                    let next = form.button as usize;
                    form.button = EditFormButton::ALL[(next + 1) % b_len];
                }
                self.screen = Screen::EditForm(form);
            }
            KeyCode::Char(' ') => {
                if form.focus == Focus::List {
                    form.auto_connect = !form.auto_connect;
                }
                self.screen = Screen::EditForm(form);
            }
            KeyCode::Enter => {
                if form.focus == Focus::List {
                    form.focus = Focus::Buttons;
                    self.screen = Screen::EditForm(form);
                } else {
                    match form.button {
                        EditFormButton::Cancel => self.screen = Screen::EditList {
                            list_idx: form.net_idx,
                            button: EditListButton::Quit,
                            focus: Focus::List,
                        },
                        EditFormButton::Save => {
                            if let Some(net) = self.known_networks.get(form.net_idx).cloned() {
                                let manager = self.iwd_manager.clone();
                                let tx = self.app_tx.clone();
                                let auto_connect = form.auto_connect;
                                tokio::spawn(async move {
                                    let res = tokio::time::timeout(
                                        std::time::Duration::from_secs(2),
                                        manager.set_auto_connect(&net.path, auto_connect),
                                    ).await;
                                    let result = match res {
                                        Ok(Ok(())) => Ok(()),
                                        Ok(Err(e)) => Err(e.to_string()),
                                        Err(_) => Err("Timeout".to_string()),
                                    };
                                    let _ = tx.send(AppEvent::ActionResult(result)).await;
                                });
                                self.status_message = Some("Saving...".into());
                                self.screen = Screen::EditList {
                                    list_idx: form.net_idx,
                                    button: EditListButton::Quit,
                                    focus: Focus::List,
                                };
                            } else {
                                self.screen = Screen::EditList {
                                    list_idx: 0,
                                    button: EditListButton::Quit,
                                    focus: Focus::List,
                                };
                            }
                        }
                        EditFormButton::Forget => {
                            if let Some(net) = self.known_networks.get(form.net_idx).cloned() {
                                let manager = self.iwd_manager.clone();
                                let tx = self.app_tx.clone();
                                tokio::spawn(async move {
                                    let res = tokio::time::timeout(
                                        std::time::Duration::from_secs(2),
                                        manager.forget_known_network(&net.path),
                                    ).await;
                                    let result = match res {
                                        Ok(Ok(())) => Ok(()),
                                        Ok(Err(e)) => Err(e.to_string()),
                                        Err(_) => Err("Timeout".to_string()),
                                    };
                                    let _ = tx.send(AppEvent::ActionResult(result)).await;
                                });
                                self.status_message = Some(format!("Forgetting '{}'", net.name));
                                self.screen = Screen::EditList {
                                    list_idx: 0,
                                    button: EditListButton::Quit,
                                    focus: Focus::List,
                                };
                            }
                        }
                    }
                }
            }
            _ => self.screen = Screen::EditForm(form),
        }
    }

    async fn handle_add_hidden_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        mut form: AddHiddenForm,
    ) {
        let b_len = AddHiddenButton::ALL.len();
        match key.code {
            KeyCode::Esc => self.screen = Screen::EditList {
                list_idx: 0,
                button: EditListButton::Quit,
                focus: Focus::List,
            },
            KeyCode::Tab | KeyCode::BackTab => {
                form.focus = if form.focus == Focus::List { Focus::Buttons } else { Focus::List };
                self.screen = Screen::AddHidden(form);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => {
                form.focus = if form.focus == Focus::List { Focus::Buttons } else { Focus::List };
                self.screen = Screen::AddHidden(form);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if form.focus == Focus::Buttons {
                    let next = form.button as usize;
                    form.button = AddHiddenButton::ALL[(next + b_len - 1) % b_len];
                }
                self.screen = Screen::AddHidden(form);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if form.focus == Focus::Buttons {
                    let next = form.button as usize;
                    form.button = AddHiddenButton::ALL[(next + 1) % b_len];
                }
                self.screen = Screen::AddHidden(form);
            }
            KeyCode::Enter => {
                if form.focus == Focus::List {
                    form.focus = Focus::Buttons;
                    self.screen = Screen::AddHidden(form);
                } else {
                    match form.button {
                        AddHiddenButton::Cancel => self.screen = Screen::EditList {
                            list_idx: 0,
                            button: EditListButton::Quit,
                            focus: Focus::List,
                        },
                        AddHiddenButton::Connect => {
                            if form.ssid.is_empty() {
                                self.error_prev_screen = Some(Box::new(Screen::AddHidden(form)));
                                self.screen = Screen::Error("SSID cannot be empty".into());
                            } else {
                                let tx = self.app_tx.clone();
                                let manager = self.iwd_manager.clone();
                                let ssid = form.ssid.clone();
                                tokio::spawn(async move {
                                    let res = manager.connect_hidden_network(&ssid).await;
                                    let _ = tx.send(AppEvent::ActionResult(res.map_err(|e| e.to_string()))).await;
                                });
                                self.status_message = Some(format!("Connecting to hidden network '{}'", form.ssid));
                                self.screen = Screen::Activate {
                                    list_idx: 0,
                                    button: ActivateButton::Quit,
                                    focus: Focus::List,
                                };
                            }
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if form.focus == Focus::List {
                    form.ssid.pop();
                }
                self.screen = Screen::AddHidden(form);
            }
            KeyCode::Char(c) => {
                if form.focus == Focus::List {
                    form.ssid.push(c);
                }
                self.screen = Screen::AddHidden(form);
            }
            _ => self.screen = Screen::AddHidden(form),
        }
    }

    fn handle_hostname_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        mut input: String,
        mut button: HostnameButton,
        focus: Focus,
    ) {
        let b_len = HostnameButton::ALL.len();
        match key.code {
            KeyCode::Esc => self.screen = Screen::Main(2),
            KeyCode::Tab | KeyCode::BackTab => {
                let new_focus = if focus == Focus::List { Focus::Buttons } else { Focus::List };
                self.screen = Screen::SetHostname { input, button, focus: new_focus };
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => {
                let new_focus = if focus == Focus::List { Focus::Buttons } else { Focus::List };
                self.screen = Screen::SetHostname { input, button, focus: new_focus };
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if focus == Focus::Buttons {
                    let next = button as usize;
                    button = HostnameButton::ALL[(next + b_len - 1) % b_len];
                }
                self.screen = Screen::SetHostname { input, button, focus };
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if focus == Focus::Buttons {
                    let next = button as usize;
                    button = HostnameButton::ALL[(next + 1) % b_len];
                }
                self.screen = Screen::SetHostname { input, button, focus };
            }
            KeyCode::Enter => {
                if focus == Focus::List {
                    self.screen = Screen::SetHostname { input, button, focus: Focus::Buttons };
                } else {
                    match button {
                        HostnameButton::Cancel => self.screen = Screen::Main(2),
                        HostnameButton::Set => match system::set_hostname(&input) {
                            Ok(()) => {
                                self.hostname = input.clone();
                                self.status_message = Some("Hostname updated".into());
                                self.screen = Screen::Main(2);
                            }
                            Err(e) => {
                                self.error_prev_screen = Some(Box::new(Screen::SetHostname { input, button, focus }));
                                self.screen = Screen::Error(e.to_string());
                            }
                        },
                    }
                }
            }
            KeyCode::Backspace => {
                if focus == Focus::List {
                    input.pop();
                }
                self.screen = Screen::SetHostname { input, button, focus };
            }
            KeyCode::Char(c) => {
                if focus == Focus::List {
                    input.push(c);
                }
                self.screen = Screen::SetHostname { input, button, focus };
            }
            _ => self.screen = Screen::SetHostname { input, button, focus },
        }
    }

    fn handle_agent_dialog_key(
        mut dialog: AgentDialog,
        key: crossterm::event::KeyEvent,
    ) -> (Option<AgentDialog>, Option<Screen>) {
        match key.code {
            KeyCode::Esc => {
                let prev = *dialog.take_prev_screen();
                dialog.cancel();
                (None, Some(prev))
            }
            KeyCode::Enter => {
                match &mut dialog {
                    AgentDialog::Passphrase { pass, .. } | AgentDialog::PrivateKeyPassphrase { pass, .. } => {
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
                        (None, Some(prev))
                    }
                    AgentDialog::UserPassword { user, pass, editing_user, .. }
                    | AgentDialog::UserNameAndPassword { user, pass, editing_user, .. } => {
                        if *editing_user {
                            *editing_user = false;
                            (Some(dialog), None)
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
                            (None, Some(prev))
                        }
                    }
                }
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Up => {
                if let AgentDialog::UserNameAndPassword { editing_user, .. } = &mut dialog {
                    if matches!(key.code, KeyCode::Up) {
                        *editing_user = true;
                    } else {
                        *editing_user = !*editing_user;
                    }
                }
                (Some(dialog), None)
            }
            KeyCode::Backspace => {
                let active = match &mut dialog {
                    AgentDialog::Passphrase { pass, .. } | AgentDialog::PrivateKeyPassphrase { pass, .. } => pass,
                    AgentDialog::UserPassword { user, pass, editing_user, .. }
                    | AgentDialog::UserNameAndPassword { user, pass, editing_user, .. } => {
                        if *editing_user { user } else { pass }
                    }
                };
                active.pop();
                (Some(dialog), None)
            }
            KeyCode::Char(c) => {
                let active = match &mut dialog {
                    AgentDialog::Passphrase { pass, .. } | AgentDialog::PrivateKeyPassphrase { pass, .. } => pass,
                    AgentDialog::UserPassword { user, pass, editing_user, .. }
                    | AgentDialog::UserNameAndPassword { user, pass, editing_user, .. } => {
                        if *editing_user { user } else { pass }
                    }
                };
                active.push(c);
                (Some(dialog), None)
            }
            _ => (Some(dialog), None),
        }
    }
}

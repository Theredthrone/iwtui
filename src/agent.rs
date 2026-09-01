//! D-Bus agent interface (`net.connman.iwd.Agent`).
//!
//! iwd calls into this object whenever it needs credentials (passphrase,
//! WPA-Enterprise user/password, private-key passphrase). The agent forwards
//! the request to the UI thread over an mpsc channel and blocks the D-Bus
//! method call until the user answers — iwd keeps the connection attempt
//! pending in the meantime.
//!
//! Signatures follow `iwd/doc/agent.txt` exactly:
//!   RequestPassphrase(o)           -> s
//!   RequestUserNameAndPassword(o)  -> ss
//!   RequestUserPassword(o, s)      -> s
//!   RequestPrivateKeyPassphrase(o) -> s
//!   Release()                      ->
//!   Cancel(o, s)                   ->

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use zbus::interface;
use zbus::zvariant::OwnedObjectPath;

use crate::iwd::NetworkProxy;

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "net.connman.iwd.Error")]
pub enum IwdAgentError {
    Canceled(String),
    Failed(String),
    #[zbus(error)]
    ZBus(zbus::Error),
}

impl From<mpsc::error::SendError<AgentRequest>> for IwdAgentError {
    fn from(e: mpsc::error::SendError<AgentRequest>) -> Self {
        IwdAgentError::Failed(format!("UI agent channel closed: {e}"))
    }
}

/// A credential prompt from iwd, waiting for the UI.
#[allow(clippy::enum_variant_names)]
pub enum AgentRequest {
    RequestPassphrase {
        network_name: String,
        reply_to: oneshot::Sender<AgentReply<String>>,
    },
    RequestUserNameAndPassword {
        network_name: String,
        reply_to: oneshot::Sender<AgentReply<(String, String)>>,
    },
    RequestUserPassword {
        network_name: String,
        user: String,
        reply_to: oneshot::Sender<AgentReply<String>>,
    },
    RequestPrivateKeyPassphrase {
        network_name: String,
        reply_to: oneshot::Sender<AgentReply<String>>,
    },
}

pub enum AgentReply<T> {
    Ok(T),
    Cancelled,
}

impl<T> AgentReply<T> {
    fn into_agent_result(self) -> Result<T, IwdAgentError> {
        match self {
            AgentReply::Ok(v) => Ok(v),
            AgentReply::Cancelled => Err(IwdAgentError::Canceled(
                "user cancelled the authentication request".into(),
            )),
        }
    }
}

pub struct IwdAgent {
    pub tx: mpsc::Sender<AgentRequest>,
    pub conn: zbus::Connection,
    /// Set when iwd cancels a pending request; the UI polls it and closes
    /// whatever dialog is on screen.
    pub cancel_flag: Arc<AtomicBool>,
}

#[interface(name = "net.connman.iwd.Agent")]
impl IwdAgent {
    async fn request_passphrase(
        &self,
        network_path: OwnedObjectPath,
    ) -> Result<String, IwdAgentError> {
        let network_name = resolve_name(&self.conn, &network_path).await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestPassphrase {
                network_name,
                reply_to: reply_tx,
            })
            .await?;
        reply_rx
            .await
            .map_err(|_| IwdAgentError::Failed("UI dropped the reply channel".into()))?
            .into_agent_result()
    }

    async fn request_user_name_and_password(
        &self,
        network_path: OwnedObjectPath,
    ) -> Result<(String, String), IwdAgentError> {
        let network_name = resolve_name(&self.conn, &network_path).await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestUserNameAndPassword {
                network_name,
                reply_to: reply_tx,
            })
            .await?;
        reply_rx
            .await
            .map_err(|_| IwdAgentError::Failed("UI dropped the reply channel".into()))?
            .into_agent_result()
    }

    async fn request_user_password(
        &self,
        network_path: OwnedObjectPath,
        user: String,
    ) -> Result<String, IwdAgentError> {
        let network_name = resolve_name(&self.conn, &network_path).await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestUserPassword {
                network_name,
                user,
                reply_to: reply_tx,
            })
            .await?;
        reply_rx
            .await
            .map_err(|_| IwdAgentError::Failed("UI dropped the reply channel".into()))?
            .into_agent_result()
    }

    async fn request_private_key_passphrase(
        &self,
        network_path: OwnedObjectPath,
    ) -> Result<String, IwdAgentError> {
        let network_name = resolve_name(&self.conn, &network_path).await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestPrivateKeyPassphrase {
                network_name,
                reply_to: reply_tx,
            })
            .await?;
        reply_rx
            .await
            .map_err(|_| IwdAgentError::Failed("UI dropped the reply channel".into()))?
            .into_agent_result()
    }

    /// iwd releases the agent when it no longer needs it (e.g. shutdown).
    /// Nothing to clean up — we unregister explicitly on exit.
    #[zbus(name = "Release")]
    fn release(&self) -> zbus::fdo::Result<()> {
        Ok(())
    }

    /// iwd calls `Cancel(object network, string reason)` with TWO arguments;
    /// a wrong signature makes zbus reject the call and the password dialog
    /// would stay stuck forever.
    #[zbus(name = "Cancel")]
    fn cancel(&self, _network_path: OwnedObjectPath, _reason: String) -> zbus::fdo::Result<()> {
        self.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// iwd only sends the network's object path — resolve its SSID so the
/// dialog can show a human-readable name.
async fn resolve_name(
    conn: &zbus::Connection,
    path: &OwnedObjectPath,
) -> Result<String, IwdAgentError> {
    let network = NetworkProxy::new(conn, path.clone()).await?;
    Ok(network
        .name()
        .await
        .unwrap_or_else(|_| "Unknown".to_string()))
}

use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
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

#[allow(clippy::enum_variant_names)]
pub enum AgentRequest {
    RequestPassphrase {
        #[allow(dead_code)]
        network_path: OwnedObjectPath,
        network_name: String,
        reply_to: oneshot::Sender<AgentReply<String>>,
    },
    RequestUserNameAndPassword {
        #[allow(dead_code)]
        network_path: OwnedObjectPath,
        network_name: String,
        reply_to: oneshot::Sender<AgentReply<(String, String)>>,
    },
    RequestUserPassword {
        #[allow(dead_code)]
        network_path: OwnedObjectPath,
        network_name: String,
        user: String,
        reply_to: oneshot::Sender<AgentReply<String>>,
    },
    RequestPrivateKeyPassphrase {
        #[allow(dead_code)]
        network_path: OwnedObjectPath,
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
                "User cancelled the authentication request".into(),
            )),
        }
    }
}

pub struct IwdAgent {
    pub tx: mpsc::Sender<AgentRequest>,
    pub conn: zbus::Connection,
    pub cancel: Arc<Notify>,
}

#[interface(name = "net.connman.iwd.Agent")]
impl IwdAgent {
    async fn request_passphrase(
        &self,
        network_path: OwnedObjectPath,
    ) -> Result<String, IwdAgentError> {
        let network = NetworkProxy::new(&self.conn, network_path.clone()).await?;
        let network_name = network.name().await.unwrap_or_else(|_| "Unknown".to_string());

        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestPassphrase {
                network_path,
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
        let network = NetworkProxy::new(&self.conn, network_path.clone()).await?;
        let network_name = network.name().await.unwrap_or_else(|_| "Unknown".to_string());

        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestUserNameAndPassword {
                network_path,
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
        let network = NetworkProxy::new(&self.conn, network_path.clone()).await?;
        let network_name = network.name().await.unwrap_or_else(|_| "Unknown".to_string());

        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestUserPassword {
                network_path,
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
        let network = NetworkProxy::new(&self.conn, network_path.clone()).await?;
        let network_name = network.name().await.unwrap_or_else(|_| "Unknown".to_string());

        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentRequest::RequestPrivateKeyPassphrase {
                network_path,
                network_name,
                reply_to: reply_tx,
            })
            .await?;
        reply_rx
            .await
            .map_err(|_| IwdAgentError::Failed("UI dropped the reply channel".into()))?
            .into_agent_result()
    }

    #[zbus(name = "Cancel")]
    fn cancel(&self) -> zbus::fdo::Result<()> {
        // Use notify_one so the cancellation permit is stored if the UI is busy
        self.cancel.notify_one();
        Ok(())
    }
}

//! WebSocket long-polling transport for Feishu.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::error::ChannelError;

use super::super::client::FeishuClient;
use super::super::types::*;

/// WebSocket connection state.
#[derive(Debug, Clone)]
pub struct WsState {
    pub connected: bool,
    pub last_heartbeat: Option<std::time::Instant>,
}

/// WebSocket transport for receiving Feishu events.
pub struct WsTransport {
    client: Arc<FeishuClient>,
    state: Arc<RwLock<WsState>>,
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

impl WsTransport {
    /// Create a new WebSocket transport.
    pub fn new(client: Arc<FeishuClient>) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(WsState {
                connected: false,
                last_heartbeat: None,
            })),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    /// Get current connection state.
    pub async fn state(&self) -> WsState {
        self.state.read().await.clone()
    }

    /// Start the WebSocket connection and return a receiver for events.
    pub async fn start(&self) -> Result<mpsc::Receiver<WsMessage>, ChannelError> {
        let (event_tx, event_rx) = mpsc::channel(100);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

        // Store shutdown sender
        {
            let mut guard = self.shutdown_tx.write().await;
            *guard = Some(shutdown_tx);
        }

        let client = Arc::clone(&self.client);
        let state = Arc::clone(&self.state);

        tokio::spawn(async move {
            let mut retry_delay = Duration::from_secs(1);
            let max_retry_delay = Duration::from_secs(30);

            loop {
                if shutdown_rx.try_recv().is_ok() {
                    tracing::info!("Feishu WebSocket: shutdown requested");
                    break;
                }

                match Self::connect_and_run(&client, &event_tx, &state, &mut shutdown_rx).await {
                    Ok(should_reconnect) => {
                        if !should_reconnect {
                            break;
                        }
                        tracing::info!(
                            "Feishu WebSocket: connection closed, reconnecting in {:?}",
                            retry_delay
                        );
                    }
                    Err(e) => {
                        tracing::error!("Feishu WebSocket: connection error: {}", e);
                    }
                }

                // Update state
                {
                    let mut guard = state.write().await;
                    guard.connected = false;
                }

                // Exponential backoff
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(max_retry_delay);
            }
        });

        Ok(event_rx)
    }

    /// Shutdown the WebSocket connection.
    pub async fn shutdown(&self) {
        let guard = self.shutdown_tx.read().await;
        if let Some(ref tx) = *guard {
            let _ = tx.send(()).await;
        }
    }

    /// Connect and run the WebSocket event loop.
    async fn connect_and_run(
        client: &FeishuClient,
        event_tx: &mpsc::Sender<WsMessage>,
        state: &Arc<RwLock<WsState>>,
        shutdown_rx: &mut mpsc::Receiver<()>,
    ) -> Result<bool, ChannelError> {
        // Get tenant token for authentication
        let token = client.get_tenant_token().await?;

        // Connect to Feishu WebSocket gateway
        let ws_url = "wss://gateway.feishu.cn/";
        let (ws_stream, _) =
            connect_async(ws_url)
                .await
                .map_err(|e| ChannelError::StartupFailed {
                    name: "feishu".to_string(),
                    reason: format!("WebSocket connect failed: {}", e),
                })?;

        tracing::info!("Feishu WebSocket: connected to gateway");

        let (mut write, mut read) = ws_stream.split();

        // Send authentication message
        let auth_msg = serde_json::json!({
            "type": "auth",
            "header": {
                "authorization": {
                    "token": token,
                }
            }
        });

        write
            .send(Message::Text(auth_msg.to_string().into()))
            .await
            .map_err(|e| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("Failed to send auth: {}", e),
            })?;

        // Wait for connected response
        let mut authenticated = false;
        let mut last_ping = std::time::Instant::now();
        let ping_interval = Duration::from_secs(20);

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<WsMessage>(&text) {
                                Ok(ws_msg) => {
                                    match ws_msg.msg_type.as_str() {
                                        "connected" => {
                                            tracing::info!("Feishu WebSocket: authenticated");
                                            authenticated = true;
                                            let mut guard = state.write().await;
                                            guard.connected = true;
                                            guard.last_heartbeat = Some(std::time::Instant::now());
                                        }
                                        "event" => {
                                            if authenticated {
                                                if event_tx.send(ws_msg).await.is_err() {
                                                    tracing::warn!("Event receiver dropped");
                                                    return Ok(false);
                                                }
                                                let mut guard = state.write().await;
                                                guard.last_heartbeat = Some(std::time::Instant::now());
                                            }
                                        }
                                        "pong" => {
                                            let mut guard = state.write().await;
                                            guard.last_heartbeat = Some(std::time::Instant::now());
                                        }
                                        "error" => {
                                            tracing::error!("Feishu WebSocket error: {:?}", ws_msg);
                                        }
                                        other => {
                                            tracing::debug!("Feishu WebSocket: unknown message type: {}", other);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse WebSocket message: {}", e);
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            tracing::info!("Feishu WebSocket: received close frame");
                            return Ok(true);
                        }
                        Some(Err(e)) => {
                            return Err(ChannelError::Disconnected {
                                name: "feishu".to_string(),
                                reason: format!("WebSocket error: {}", e),
                            });
                        }
                        None => {
                            tracing::info!("Feishu WebSocket: stream ended");
                            return Ok(true);
                        }
                        _ => {}
                    }
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("Feishu WebSocket: shutdown requested during event loop");
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(false);
                }
                _ = tokio::time::sleep(ping_interval) => {
                    if last_ping.elapsed() >= ping_interval {
                        let ping_msg = serde_json::json!({
                            "type": "ping"
                        });
                        if write
                            .send(Message::Text(ping_msg.to_string().into()))
                            .await
                            .is_err()
                        {
                            tracing::warn!("Failed to send ping");
                            return Ok(true);
                        }
                        last_ping = std::time::Instant::now();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[tokio::test]
    async fn ws_transport_creation() {
        let client = Arc::new(
            FeishuClient::new(
                "cli_test".to_string(),
                Some(SecretString::from("secret".to_string())),
                "https://open.feishu.cn".to_string(),
            )
            .unwrap(),
        );
        let transport = WsTransport::new(client);
        let state = transport.state().await;
        assert!(!state.connected);
    }
}

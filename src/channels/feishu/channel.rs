//! Feishu channel implementation.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use secrecy::SecretString;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use crate::channels::channel::{
    Channel, ChannelSecretUpdater, IncomingMessage, MessageStream, OutgoingResponse, StatusUpdate,
};
use crate::config::FeishuConfig;
use crate::error::ChannelError;

use super::client::FeishuClient;
use super::mention;
use super::session::{self, GroupSessionScope};
use super::transport::websocket::WsTransport;
use super::types::*;

/// Deduplication cache for message IDs.
struct DedupCache {
    ids: std::collections::HashSet<String>,
    max_size: usize,
}

impl DedupCache {
    fn new(max_size: usize) -> Self {
        Self {
            ids: std::collections::HashSet::new(),
            max_size,
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    fn insert(&mut self, id: String) {
        if self.ids.len() >= self.max_size {
            // Remove oldest entries (simple approach: clear half)
            let to_remove: Vec<String> = self.ids.iter().take(self.max_size / 2).cloned().collect();
            for id in to_remove {
                self.ids.remove(&id);
            }
        }
        self.ids.insert(id);
    }
}

/// Feishu channel state.
pub struct FeishuChannel {
    config: FeishuConfig,
    client: Arc<FeishuClient>,
    transport: WsTransport,
    bot_open_id: Arc<RwLock<Option<String>>>,
    bot_name: Arc<RwLock<Option<String>>>,
    group_session_scope: GroupSessionScope,
    dedup: Arc<RwLock<DedupCache>>,
    connected: Arc<RwLock<bool>>,
}

impl FeishuChannel {
    /// Create a new Feishu channel.
    pub async fn new(config: FeishuConfig) -> Result<Self, ChannelError> {
        if !config.is_configured() {
            return Err(ChannelError::InvalidMessage(
                "Feishu app_id and app_secret are required".to_string(),
            ));
        }

        let client = Arc::new(FeishuClient::new(
            config.app_id.clone(),
            config.app_secret.clone(),
            config.domain.clone(),
        )?);

        let transport = WsTransport::new(Arc::clone(&client));
        let group_session_scope = GroupSessionScope::parse(&config.group_session_scope);

        Ok(Self {
            config,
            client,
            transport,
            bot_open_id: Arc::new(RwLock::new(None)),
            bot_name: Arc::new(RwLock::new(None)),
            group_session_scope,
            dedup: Arc::new(RwLock::new(DedupCache::new(1000))),
            connected: Arc::new(RwLock::new(false)),
        })
    }

    /// Fetch and cache bot info.
    async fn fetch_bot_info(&self) -> Result<(), ChannelError> {
        let response = self.client.get_bot_info().await?;

        if response.code != 0 {
            return Err(ChannelError::StartupFailed {
                name: "feishu".to_string(),
                reason: format!(
                    "Failed to get bot info: code={}, msg={}",
                    response.code, response.msg
                ),
            });
        }

        if let Some(bot) = response.bot {
            if let Some(open_id) = bot.open_id {
                tracing::info!("Feishu bot open_id: {}", open_id);
                let mut guard = self.bot_open_id.write().await;
                *guard = Some(open_id);
            }
            if let Some(name) = bot.name {
                tracing::info!("Feishu bot name: {}", name);
                let mut guard = self.bot_name.write().await;
                *guard = Some(name);
            }
        }

        Ok(())
    }

    /// Get bot open_id (cached).
    async fn get_bot_open_id(&self) -> Option<String> {
        self.bot_open_id.read().await.clone()
    }

    /// Parse a WebSocket event into an IncomingMessage.
    async fn parse_event(&self, event: WsMessage) -> Option<IncomingMessage> {
        let header = event.header?;
        let event_type = header.event_type?;

        if event_type != "im.message.receive_v1" {
            tracing::debug!("Ignoring event type: {}", event_type);
            return None;
        }

        let event_data = event.event?;
        let receive_event: ReceiveMessageEvent = match serde_json::from_value(event_data) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to parse receive event: {}", e);
                return None;
            }
        };

        let message = receive_event.message?;
        let message_id = message.message_id?;

        // Dedup check
        {
            let mut dedup = self.dedup.write().await;
            if dedup.contains(&message_id) {
                tracing::debug!("Duplicate message: {}", message_id);
                return None;
            }
            dedup.insert(message_id.clone());
        }

        let sender = receive_event.sender?;
        let sender_id = sender
            .sender_id
            .as_ref()
            .and_then(|id| id.open_id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let chat_id = receive_event
            .chat_id
            .or_else(|| message.chat_id.clone())
            .unwrap_or_default();

        let chat_type = if chat_id.starts_with("ou_") {
            "p2p"
        } else {
            "group"
        };

        // Extract text content
        let body = message.body?;
        let content_str = body.content.unwrap_or_default();
        let mut text = String::new();

        // Parse message content based on type
        let msg_type = message.msg_type.as_deref().unwrap_or("text");
        match msg_type {
            "text" => {
                if let Ok(content) = serde_json::from_str::<TextContent>(&content_str) {
                    text = content.text;
                }
            }
            "post" => {
                // Extract text from post (rich text)
                text = self.extract_text_from_post(&content_str);
            }
            "interactive" => {
                // Card messages - extract text if possible
                text = "[Card Message]".to_string();
            }
            "image" => {
                text = "[Image]".to_string();
            }
            "file" => {
                text = "[File]".to_string();
            }
            "audio" => {
                text = "[Audio]".to_string();
            }
            "sticker" => {
                text = "[Sticker]".to_string();
            }
            _ => {
                text = format!("[{}]", msg_type);
            }
        }

        // Check mentions
        let bot_open_id = self.get_bot_open_id().await.unwrap_or_default();
        let is_mentioned = mention::is_bot_mentioned(&message.mentions, &bot_open_id);

        // Check if we should respond based on policy
        let should_respond =
            mention::should_respond(chat_type, self.config.require_mention, is_mentioned, &text);

        if !should_respond {
            tracing::debug!(
                "Skipping message {} - not mentioned and require_mention is true",
                message_id
            );
            return None;
        }

        // Compute session key
        let session_key = session::compute_session_key(
            &chat_id,
            chat_type,
            Some(&sender_id),
            message.thread_id.as_deref(),
            &self.group_session_scope,
        );

        // Strip mentions from text
        let clean_text = mention::strip_mentions(&text);

        Some(IncomingMessage {
            id: Uuid::new_v4(),
            channel: "feishu".to_string(),
            user_id: sender_id,
            user_name: None,
            content: clean_text,
            thread_id: message.thread_id,
            received_at: Utc::now(),
            metadata: serde_json::json!({
                "message_id": message_id,
                "chat_id": chat_id,
                "chat_type": chat_type,
                "session_key": session_key,
                "is_mentioned": is_mentioned,
                "msg_type": msg_type,
                "raw_content": content_str,
            }),
            timezone: None,
            attachments: Vec::new(),
        })
    }

    /// Extract text from a post (rich text) message.
    fn extract_text_from_post(&self, content: &str) -> String {
        if let Ok(post) = serde_json::from_str::<PostContent>(content) {
            let mut text = String::new();
            if let Some(zh_cn) = post.post.zh_cn {
                for paragraph in zh_cn.content {
                    for element in paragraph {
                        match element {
                            PostElement::Text { text: t } => text.push_str(&t),
                            PostElement::Link { text: t, .. } => text.push_str(&t),
                            PostElement::At {
                                user_name: Some(name),
                                ..
                            } => {
                                text.push_str(&format!("@{}", name));
                            }
                            PostElement::Emotion { emoji_type } => {
                                text.push_str(&emoji_type);
                            }
                            _ => {}
                        }
                    }
                    text.push('\n');
                }
            }
            text.trim().to_string()
        } else {
            content.to_string()
        }
    }

    /// Get the receive_id and type for sending a response.
    fn get_send_target(&self, metadata: &serde_json::Value) -> Option<(String, String)> {
        let chat_type = metadata.get("chat_type")?.as_str()?;
        let chat_id = metadata.get("chat_id")?.as_str()?;
        let user_id = metadata.get("user_id")?.as_str()?;

        let (id_type, id) = match chat_type {
            "p2p" => ("open_id", user_id),
            _ => ("chat_id", chat_id),
        };

        Some((id_type.to_string(), id.to_string()))
    }
}

#[async_trait]
impl Channel for FeishuChannel {
    fn name(&self) -> &str {
        "feishu"
    }

    async fn start(&self) -> Result<MessageStream, ChannelError> {
        tracing::info!("Starting Feishu channel");

        // Fetch bot info first
        self.fetch_bot_info().await?;

        // Start WebSocket transport
        let mut event_rx = self.transport.start().await?;

        let (msg_tx, msg_rx) = mpsc::channel(100);
        let channel = self.clone_for_event_loop();

        tokio::spawn(async move {
            *channel.connected.write().await = true;
            tracing::info!("Feishu channel event loop started");

            while let Some(event) = event_rx.recv().await {
                if let Some(msg) = channel.parse_event(event).await
                    && msg_tx.send(msg).await.is_err()
                {
                    tracing::warn!("Message receiver dropped");
                    break;
                }
            }

            *channel.connected.write().await = false;
            tracing::info!("Feishu channel event loop ended");
        });

        // Convert mpsc::Receiver to Stream
        let stream = tokio_stream::wrappers::ReceiverStream::new(msg_rx);
        Ok(Box::pin(stream))
    }

    async fn respond(
        &self,
        msg: &IncomingMessage,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        let metadata = &msg.metadata;
        let (receive_id_type, receive_id) =
            self.get_send_target(metadata)
                .ok_or_else(|| ChannelError::SendFailed {
                    name: "feishu".to_string(),
                    reason: "Missing chat_id or user_id in metadata".to_string(),
                })?;

        let message_id = metadata
            .get("message_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Check if we should reply or send a new message
        if let Some(msg_id) = message_id {
            // Reply to the original message
            if response.content.starts_with("[Card]") {
                // Build a card from the content
                let card = build_text_card(&response.content);
                super::send::reply_card(&self.client, &msg_id, &card).await?;
            } else {
                super::send::reply_text(&self.client, &msg_id, &response.content).await?;
            }
        } else {
            // Send a new message
            if response.content.starts_with("[Card]") {
                let card = build_text_card(&response.content);
                super::send::send_card(&self.client, &receive_id_type, &receive_id, &card).await?;
            } else {
                super::send::send_text(
                    &self.client,
                    &receive_id_type,
                    &receive_id,
                    &response.content,
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn send_status(
        &self,
        status: StatusUpdate,
        metadata: &serde_json::Value,
    ) -> Result<(), ChannelError> {
        let (receive_id_type, receive_id) =
            self.get_send_target(metadata)
                .ok_or_else(|| ChannelError::SendFailed {
                    name: "feishu".to_string(),
                    reason: "Missing chat_id or user_id in metadata".to_string(),
                })?;

        let status_text = match &status {
            StatusUpdate::Thinking(s) => s.clone(),
            StatusUpdate::ToolStarted { name } => format!("Executing {}...", name),
            StatusUpdate::ToolCompleted {
                name,
                success,
                error,
                ..
            } => {
                if *success {
                    format!("{} completed", name)
                } else {
                    format!(
                        "{} failed: {}",
                        name,
                        error.as_deref().unwrap_or("unknown error")
                    )
                }
            }
            StatusUpdate::Status(s) => s.clone(),
            _ => return Ok(()), // Skip other status types for Feishu
        };

        let card = CardContent {
            config: Some(CardConfig {
                wide_screen_mode: true,
                enable_forward: false,
            }),
            header: None,
            elements: vec![CardElement::Div {
                text: Some(CardText {
                    tag: "plain_text".to_string(),
                    content: status_text,
                    text_color: Some("grey".to_string()),
                }),
                fields: vec![],
            }],
        };

        super::send::send_card(&self.client, &receive_id_type, &receive_id, &card).await?;
        Ok(())
    }

    async fn broadcast(
        &self,
        user_id: &str,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        // Parse user_id format: "user:open_id" or "chat:chat_id"
        let (id_type, id) = if let Some(rest) = user_id.strip_prefix("user:") {
            ("open_id", rest)
        } else if let Some(rest) = user_id.strip_prefix("chat:") {
            ("chat_id", rest)
        } else {
            ("open_id", user_id)
        };

        super::send::send_text(&self.client, id_type, id, &response.content).await?;
        Ok(())
    }

    async fn health_check(&self) -> Result<(), ChannelError> {
        let state = self.transport.state().await;

        if state.connected {
            Ok(())
        } else {
            Err(ChannelError::HealthCheckFailed {
                name: "feishu".to_string(),
            })
        }
    }

    fn conversation_context(&self, metadata: &serde_json::Value) -> HashMap<String, String> {
        let mut ctx = HashMap::new();

        if let Some(chat_type) = metadata.get("chat_type").and_then(|v| v.as_str()) {
            ctx.insert("chat_type".to_string(), chat_type.to_string());
        }
        if let Some(session_key) = metadata.get("session_key").and_then(|v| v.as_str()) {
            ctx.insert("session".to_string(), session_key.to_string());
        }
        if let Some(is_mentioned) = metadata.get("is_mentioned").and_then(|v| v.as_bool()) {
            ctx.insert("is_mentioned".to_string(), is_mentioned.to_string());
        }

        ctx
    }

    async fn shutdown(&self) -> Result<(), ChannelError> {
        tracing::info!("Shutting down Feishu channel");
        self.transport.shutdown().await;
        Ok(())
    }
}

#[async_trait]
impl ChannelSecretUpdater for FeishuChannel {
    async fn update_secret(&self, new_secret: Option<SecretString>) {
        if let Some(secret) = new_secret {
            self.client.update_app_secret(secret).await;
            tracing::info!("Feishu app_secret updated via SIGHUP");
        }
    }
}

impl FeishuChannel {
    /// Clone for event loop (only what's needed).
    fn clone_for_event_loop(&self) -> Self {
        Self {
            config: self.config.clone(),
            client: Arc::clone(&self.client),
            transport: WsTransport::new(Arc::clone(&self.client)),
            bot_open_id: Arc::clone(&self.bot_open_id),
            bot_name: Arc::clone(&self.bot_name),
            group_session_scope: self.group_session_scope.clone(),
            dedup: Arc::clone(&self.dedup),
            connected: Arc::clone(&self.connected),
        }
    }
}

/// Build a simple text card for displaying long text.
fn build_text_card(text: &str) -> CardContent {
    // Remove [Card] prefix if present
    let text = text.strip_prefix("[Card]").unwrap_or(text).trim();

    CardContent {
        config: Some(CardConfig {
            wide_screen_mode: true,
            enable_forward: true,
        }),
        header: Some(CardHeader {
            title: CardTitle {
                tag: "plain_text".to_string(),
                content: "Response".to_string(),
            },
            template: Some("blue".to_string()),
        }),
        elements: vec![CardElement::Markdown {
            content: text.to_string(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    fn test_config() -> FeishuConfig {
        FeishuConfig {
            app_id: "cli_test".to_string(),
            app_secret: Some(SecretString::from("secret".to_string())),
            domain: "https://open.feishu.cn".to_string(),
            connection_mode: "websocket".to_string(),
            dm_policy: "allow".to_string(),
            group_policy: "allow".to_string(),
            allow_from: vec![],
            allow_from_groups: vec![],
            group_session_scope: "group_sender".to_string(),
            reply_in_thread: false,
            require_mention: true,
        }
    }

    #[test]
    fn build_text_card_works() {
        let card = build_text_card("Hello, world!");
        assert!(card.config.is_some());
        assert!(card.header.is_some());

        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("Hello, world!"));
    }

    #[test]
    fn build_text_card_strips_prefix() {
        let card = build_text_card("[Card]Hello, world!");
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("Hello, world!"));
        assert!(!json.contains("[Card]"));
    }

    #[tokio::test]
    async fn channel_name() {
        let config = test_config();
        let channel = FeishuChannel::new(config).await.unwrap();
        assert_eq!(channel.name(), "feishu");
    }

    #[tokio::test]
    async fn channel_unconfigured() {
        let config = FeishuConfig {
            app_id: "".to_string(),
            app_secret: None,
            ..test_config()
        };
        let result = FeishuChannel::new(config).await;
        assert!(result.is_err());
    }
}

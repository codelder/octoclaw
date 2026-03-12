//! Message sending for Feishu channel.

use std::sync::Arc;

use crate::error::ChannelError;

use super::client::FeishuClient;
use super::types::*;

/// Send a text message.
pub async fn send_text(
    client: &Arc<FeishuClient>,
    receive_id_type: &str,
    receive_id: &str,
    text: &str,
) -> Result<String, ChannelError> {
    let content = serde_json::to_string(&TextContent {
        text: text.to_string(),
    })
    .map_err(|e| ChannelError::SendFailed {
        name: "feishu".to_string(),
        reason: format!("Failed to serialize text content: {}", e),
    })?;

    let response = client
        .send_message(receive_id_type, receive_id, &content, "text")
        .await?;

    if response.code != 0 {
        return Err(ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!(
                "Send message failed: code={}, msg={}",
                response.code, response.msg
            ),
        });
    }

    response
        .data
        .and_then(|d| d.message_id)
        .ok_or_else(|| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: "No message_id in response".to_string(),
        })
}

/// Reply to a message with text.
pub async fn reply_text(
    client: &Arc<FeishuClient>,
    message_id: &str,
    text: &str,
) -> Result<String, ChannelError> {
    let content = serde_json::to_string(&TextContent {
        text: text.to_string(),
    })
    .map_err(|e| ChannelError::SendFailed {
        name: "feishu".to_string(),
        reason: format!("Failed to serialize text content: {}", e),
    })?;

    let response = client.reply_message(message_id, &content, "text").await?;

    if response.code != 0 {
        return Err(ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!(
                "Reply message failed: code={}, msg={}",
                response.code, response.msg
            ),
        });
    }

    response
        .data
        .and_then(|d| d.message_id)
        .ok_or_else(|| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: "No message_id in response".to_string(),
        })
}

/// Send an interactive card message.
pub async fn send_card(
    client: &Arc<FeishuClient>,
    receive_id_type: &str,
    receive_id: &str,
    card: &CardContent,
) -> Result<String, ChannelError> {
    let content = serde_json::to_string(card).map_err(|e| ChannelError::SendFailed {
        name: "feishu".to_string(),
        reason: format!("Failed to serialize card content: {}", e),
    })?;

    let response = client
        .send_message(receive_id_type, receive_id, &content, "interactive")
        .await?;

    if response.code != 0 {
        return Err(ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!(
                "Send card failed: code={}, msg={}",
                response.code, response.msg
            ),
        });
    }

    response
        .data
        .and_then(|d| d.message_id)
        .ok_or_else(|| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: "No message_id in response".to_string(),
        })
}

/// Reply with an interactive card.
pub async fn reply_card(
    client: &Arc<FeishuClient>,
    message_id: &str,
    card: &CardContent,
) -> Result<String, ChannelError> {
    let content = serde_json::to_string(card).map_err(|e| ChannelError::SendFailed {
        name: "feishu".to_string(),
        reason: format!("Failed to serialize card content: {}", e),
    })?;

    let response = client
        .reply_message(message_id, &content, "interactive")
        .await?;

    if response.code != 0 {
        return Err(ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!(
                "Reply card failed: code={}, msg={}",
                response.code, response.msg
            ),
        });
    }

    response
        .data
        .and_then(|d| d.message_id)
        .ok_or_else(|| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: "No message_id in response".to_string(),
        })
}

/// Send an image message.
pub async fn send_image(
    client: &Arc<FeishuClient>,
    receive_id_type: &str,
    receive_id: &str,
    image_key: &str,
) -> Result<String, ChannelError> {
    let content = serde_json::json!({
        "image_key": image_key
    })
    .to_string();

    let response = client
        .send_message(receive_id_type, receive_id, &content, "image")
        .await?;

    if response.code != 0 {
        return Err(ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!(
                "Send image failed: code={}, msg={}",
                response.code, response.msg
            ),
        });
    }

    response
        .data
        .and_then(|d| d.message_id)
        .ok_or_else(|| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: "No message_id in response".to_string(),
        })
}

/// Send a file message.
pub async fn send_file(
    client: &Arc<FeishuClient>,
    receive_id_type: &str,
    receive_id: &str,
    file_key: &str,
) -> Result<String, ChannelError> {
    let content = serde_json::json!({
        "file_key": file_key
    })
    .to_string();

    let response = client
        .send_message(receive_id_type, receive_id, &content, "file")
        .await?;

    if response.code != 0 {
        return Err(ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!(
                "Send file failed: code={}, msg={}",
                response.code, response.msg
            ),
        });
    }

    response
        .data
        .and_then(|d| d.message_id)
        .ok_or_else(|| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: "No message_id in response".to_string(),
        })
}

/// Send a "thinking" status as a card update.
pub async fn send_thinking_card(
    client: &Arc<FeishuClient>,
    receive_id_type: &str,
    receive_id: &str,
) -> Result<String, ChannelError> {
    let card = CardContent {
        config: Some(CardConfig {
            wide_screen_mode: true,
            enable_forward: false,
        }),
        header: None,
        elements: vec![CardElement::Div {
            text: Some(CardText {
                tag: "plain_text".to_string(),
                content: "Thinking...".to_string(),
                text_color: Some("grey".to_string()),
            }),
            fields: vec![],
        }],
    };

    send_card(client, receive_id_type, receive_id, &card).await
}

/// Update a card message (for streaming/thinking status).
pub async fn update_card(
    client: &Arc<FeishuClient>,
    message_id: &str,
    card: &CardContent,
) -> Result<(), ChannelError> {
    let _token = client.get_tenant_token().await?;
    let _content = serde_json::to_string(card).map_err(|e| ChannelError::SendFailed {
        name: "feishu".to_string(),
        reason: format!("Failed to serialize card content: {}", e),
    })?;

    // Note: Feishu doesn't have a direct card update API in standard mode
    // This would require using the message update API if available
    // For now, we just log this limitation
    tracing::debug!(
        "Card update requested for message {} but not implemented in standard mode",
        message_id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_card_for_thinking() {
        let card = CardContent {
            config: Some(CardConfig {
                wide_screen_mode: true,
                enable_forward: false,
            }),
            header: None,
            elements: vec![CardElement::Div {
                text: Some(CardText {
                    tag: "plain_text".to_string(),
                    content: "Processing...".to_string(),
                    text_color: Some("grey".to_string()),
                }),
                fields: vec![],
            }],
        };
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("Processing..."));
    }
}

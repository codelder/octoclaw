//! Feishu API client with tenant token caching.

use std::sync::Arc;
use std::time::{Duration, Instant};

use secrecy::{ExposeSecret, SecretString};
use tokio::sync::RwLock;

use crate::error::ChannelError;

use super::types::*;

/// Cached tenant access token.
#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: Instant,
}

/// Feishu API client with automatic token management.
#[derive(Clone)]
pub struct FeishuClient {
    http: reqwest::Client,
    base_url: String,
    app_id: String,
    app_secret: Arc<RwLock<Option<SecretString>>>,
    tenant_token: Arc<RwLock<Option<CachedToken>>>,
}

impl FeishuClient {
    /// Create a new Feishu client.
    pub fn new(
        app_id: String,
        app_secret: Option<SecretString>,
        domain: String,
    ) -> Result<Self, ChannelError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ChannelError::StartupFailed {
                name: "feishu".to_string(),
                reason: format!("Failed to create HTTP client: {}", e),
            })?;

        Ok(Self {
            http,
            base_url: domain.trim_end_matches('/').to_string(),
            app_id,
            app_secret: Arc::new(RwLock::new(app_secret)),
            tenant_token: Arc::new(RwLock::new(None)),
        })
    }

    /// Update the app secret (for SIGHUP hot reload).
    pub async fn update_app_secret(&self, secret: SecretString) {
        let mut guard = self.app_secret.write().await;
        *guard = Some(secret);
        // Invalidate cached token so next call fetches a new one
        let mut token_guard = self.tenant_token.write().await;
        *token_guard = None;
        tracing::info!("Feishu app_secret updated, token cache invalidated");
    }

    /// Get a valid tenant access token, fetching a new one if needed.
    pub async fn get_tenant_token(&self) -> Result<String, ChannelError> {
        // Check if we have a valid cached token
        {
            let guard = self.tenant_token.read().await;
            if let Some(ref cached) = *guard {
                // Refresh 5 minutes before expiry
                if cached.expires_at > Instant::now() + Duration::from_secs(300) {
                    return Ok(cached.token.clone());
                }
            }
        }

        // Fetch a new token
        let app_secret = {
            let guard = self.app_secret.read().await;
            guard
                .as_ref()
                .ok_or_else(|| {
                    ChannelError::InvalidMessage("FEISHU_APP_SECRET is not configured".to_string())
                })?
                .expose_secret()
                .to_string()
        };

        let url = format!("{}/auth/v3/tenant_access_token/internal", self.base_url);

        let body = serde_json::json!({
            "app_id": self.app_id,
            "app_secret": app_secret,
        });

        let response = self.http.post(&url).json(&body).send().await.map_err(|e| {
            ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("Failed to fetch tenant token: {}", e),
            }
        })?;

        let token_response: TenantAccessTokenResponse =
            response
                .json()
                .await
                .map_err(|e| ChannelError::SendFailed {
                    name: "feishu".to_string(),
                    reason: format!("Failed to parse token response: {}", e),
                })?;

        if token_response.code != 0 {
            return Err(ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!(
                    "Failed to get tenant token: code={}, msg={}",
                    token_response.code, token_response.msg
                ),
            });
        }

        let token = token_response
            .tenant_access_token
            .ok_or_else(|| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: "No tenant_access_token in response".to_string(),
            })?;

        let expires_in = token_response.expire.unwrap_or(7200);

        // Cache the token
        let cached = CachedToken {
            token: token.clone(),
            expires_at: Instant::now() + Duration::from_secs(expires_in),
        };

        let mut guard = self.tenant_token.write().await;
        *guard = Some(cached);

        Ok(token)
    }

    /// Make an authenticated GET request.
    pub async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ChannelError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}{}", self.base_url, path);

        let response = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("GET {} failed: {}", path, e),
            })?;

        response.json().await.map_err(|e| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!("Failed to parse response from {}: {}", path, e),
        })
    }

    /// Make an authenticated POST request.
    pub async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ChannelError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}{}", self.base_url, path);

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(body)
            .send()
            .await
            .map_err(|e| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("POST {} failed: {}", path, e),
            })?;

        response.json().await.map_err(|e| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!("Failed to parse response from {}: {}", path, e),
        })
    }

    /// Send a message.
    pub async fn send_message(
        &self,
        receive_id_type: &str,
        receive_id: &str,
        content: &str,
        msg_type: &str,
    ) -> Result<MessageResponse, ChannelError> {
        let path = format!("/im/v1/messages?receive_id_type={}", receive_id_type);
        let body = serde_json::json!({
            "receive_id": receive_id,
            "msg_type": msg_type,
            "content": content,
        });

        self.post(&path, &body).await
    }

    /// Reply to a message.
    pub async fn reply_message(
        &self,
        message_id: &str,
        content: &str,
        msg_type: &str,
    ) -> Result<MessageResponse, ChannelError> {
        let path = format!("/im/v1/messages/{}/reply", message_id);
        let body = serde_json::json!({
            "msg_type": msg_type,
            "content": content,
        });

        self.post(&path, &body).await
    }

    /// Get a message by ID.
    pub async fn get_message(&self, message_id: &str) -> Result<GetMessageResponse, ChannelError> {
        let path = format!("/im/v1/messages/{}", message_id);
        self.get(&path).await
    }

    /// Upload a file.
    pub async fn upload_file(
        &self,
        file_type: &str,
        file_name: &str,
        data: Vec<u8>,
    ) -> Result<UploadFileResponse, ChannelError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/files", self.base_url);

        let part = reqwest::multipart::Part::bytes(data)
            .file_name(file_name.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("Failed to create multipart: {}", e),
            })?;

        let form = reqwest::multipart::Form::new()
            .text("file_type", file_type.to_string())
            .text("file_name", file_name.to_string())
            .part("file", part);

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("Failed to upload file: {}", e),
            })?;

        response.json().await.map_err(|e| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!("Failed to parse upload response: {}", e),
        })
    }

    /// Upload an image.
    pub async fn upload_image(
        &self,
        image_type: &str,
        data: Vec<u8>,
    ) -> Result<UploadFileResponse, ChannelError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/images", self.base_url);

        let part = reqwest::multipart::Part::bytes(data)
            .file_name("image.png")
            .mime_str("image/png")
            .map_err(|e| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("Failed to create multipart: {}", e),
            })?;

        let form = reqwest::multipart::Form::new()
            .text("image_type", image_type.to_string())
            .part("image", part);

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!("Failed to upload image: {}", e),
            })?;

        response.json().await.map_err(|e| ChannelError::SendFailed {
            name: "feishu".to_string(),
            reason: format!("Failed to parse upload response: {}", e),
        })
    }

    /// Add a reaction to a message.
    pub async fn add_reaction(
        &self,
        message_id: &str,
        emoji_type: &str,
    ) -> Result<AddReactionResponse, ChannelError> {
        let path = format!("/im/v1/messages/{}/reactions", message_id);
        let body = serde_json::json!({
            "reaction_type": {
                "emoji_type": emoji_type,
            }
        });

        self.post(&path, &body).await
    }

    /// Get bot info.
    pub async fn get_bot_info(&self) -> Result<BotInfoResponse, ChannelError> {
        self.get("/bot/v3/info").await
    }

    /// Get user info.
    pub async fn get_user_info(
        &self,
        user_id: &str,
        user_id_type: &str,
    ) -> Result<UserInfoResponse, ChannelError> {
        let path = format!(
            "/contact/v3/users/{}?user_id_type={}",
            user_id, user_id_type
        );
        self.get(&path).await
    }

    /// Get chat info.
    pub async fn get_chat_info(&self, chat_id: &str) -> Result<serde_json::Value, ChannelError> {
        let path = format!("/im/v1/chats/{}", chat_id);
        self.get(&path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_creation() {
        let client = FeishuClient::new(
            "cli_test".to_string(),
            Some(SecretString::from("secret".to_string())),
            "https://open.feishu.cn".to_string(),
        );
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn client_creation_without_secret() {
        let client = FeishuClient::new(
            "cli_test".to_string(),
            None,
            "https://open.feishu.cn".to_string(),
        );
        assert!(client.is_ok());
        // Token fetch should fail without secret
        let result = client.unwrap().get_tenant_token().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn domain_trailing_slash() {
        let client = FeishuClient::new(
            "cli_test".to_string(),
            None,
            "https://open.feishu.cn/".to_string(),
        )
        .unwrap();
        assert_eq!(client.base_url, "https://open.feishu.cn");
    }
}

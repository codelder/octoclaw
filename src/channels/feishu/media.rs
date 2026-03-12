//! Media upload handling for Feishu.

use std::sync::Arc;

use crate::error::ChannelError;

use super::client::FeishuClient;

/// Media types supported by Feishu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeishuMediaType {
    Image,
    File,
    Audio,
}

impl FeishuMediaType {
    /// Get the API file_type parameter value.
    pub fn as_file_type(&self) -> &'static str {
        match self {
            FeishuMediaType::Image => "image",
            FeishuMediaType::File => "stream",
            FeishuMediaType::Audio => "opus",
        }
    }

    /// Get the image_type for image uploads.
    pub fn as_image_type(&self) -> &'static str {
        match self {
            FeishuMediaType::Image => "message",
            _ => "message",
        }
    }

    /// Detect media type from MIME type.
    pub fn from_mime(mime: &str) -> Option<Self> {
        let mime = mime.to_lowercase();
        if mime.starts_with("image/") {
            Some(FeishuMediaType::Image)
        } else if mime.starts_with("audio/") {
            Some(FeishuMediaType::Audio)
        } else {
            Some(FeishuMediaType::File)
        }
    }

    /// Detect media type from file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" => Some(FeishuMediaType::Image),
            "mp3" | "wav" | "ogg" | "m4a" | "opus" => Some(FeishuMediaType::Audio),
            _ => Some(FeishuMediaType::File),
        }
    }
}

/// Upload a file to Feishu and return the file key.
pub async fn upload_file(
    client: &Arc<FeishuClient>,
    media_type: FeishuMediaType,
    file_name: &str,
    data: Vec<u8>,
) -> Result<String, ChannelError> {
    if media_type == FeishuMediaType::Image {
        let response = client
            .upload_image(media_type.as_image_type(), data)
            .await?;

        if response.code != 0 {
            return Err(ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!(
                    "Image upload failed: code={}, msg={}",
                    response.code, response.msg
                ),
            });
        }

        response
            .data
            .and_then(|d| d.image_key)
            .ok_or_else(|| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: "No image_key in upload response".to_string(),
            })
    } else {
        let response = client
            .upload_file(media_type.as_file_type(), file_name, data)
            .await?;

        if response.code != 0 {
            return Err(ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: format!(
                    "File upload failed: code={}, msg={}",
                    response.code, response.msg
                ),
            });
        }

        response
            .data
            .and_then(|d| d.file_key)
            .ok_or_else(|| ChannelError::SendFailed {
                name: "feishu".to_string(),
                reason: "No file_key in upload response".to_string(),
            })
    }
}

/// Build image message content JSON.
pub fn build_image_content(image_key: &str) -> String {
    serde_json::json!({
        "image_key": image_key
    })
    .to_string()
}

/// Build file message content JSON.
pub fn build_file_content(file_key: &str, file_name: Option<&str>) -> String {
    let mut content = serde_json::json!({
        "file_key": file_key
    });
    if let Some(name) = file_name {
        content["file_name"] = serde_json::Value::String(name.to_string());
    }
    content.to_string()
}

/// Build audio message content JSON.
pub fn build_audio_content(file_key: &str) -> String {
    serde_json::json!({
        "file_key": file_key
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_type_from_mime() {
        assert_eq!(
            FeishuMediaType::from_mime("image/png"),
            Some(FeishuMediaType::Image)
        );
        assert_eq!(
            FeishuMediaType::from_mime("audio/mp3"),
            Some(FeishuMediaType::Audio)
        );
        assert_eq!(
            FeishuMediaType::from_mime("application/pdf"),
            Some(FeishuMediaType::File)
        );
    }

    #[test]
    fn media_type_from_extension() {
        assert_eq!(
            FeishuMediaType::from_extension("jpg"),
            Some(FeishuMediaType::Image)
        );
        assert_eq!(
            FeishuMediaType::from_extension("mp3"),
            Some(FeishuMediaType::Audio)
        );
        assert_eq!(
            FeishuMediaType::from_extension("pdf"),
            Some(FeishuMediaType::File)
        );
    }

    #[test]
    fn file_type_mapping() {
        assert_eq!(FeishuMediaType::Image.as_file_type(), "image");
        assert_eq!(FeishuMediaType::File.as_file_type(), "stream");
        assert_eq!(FeishuMediaType::Audio.as_file_type(), "opus");
    }

    #[test]
    fn build_image_content_works() {
        let content = build_image_content("img_123");
        assert!(content.contains("img_123"));
        assert!(content.contains("image_key"));
    }

    #[test]
    fn build_file_content_works() {
        let content = build_file_content("file_123", Some("test.pdf"));
        assert!(content.contains("file_123"));
        assert!(content.contains("test.pdf"));
    }

    #[test]
    fn build_file_content_no_name() {
        let content = build_file_content("file_123", None);
        assert!(content.contains("file_123"));
        assert!(!content.contains("file_name"));
    }
}

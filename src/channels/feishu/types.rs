//! Feishu/Lark API types.

use serde::{Deserialize, Serialize};

/// Tenant access token response.
#[derive(Debug, Clone, Deserialize)]
pub struct TenantAccessTokenResponse {
    pub code: i32,
    pub msg: String,
    #[serde(default)]
    pub tenant_access_token: Option<String>,
    #[serde(default)]
    pub expire: Option<u64>,
}

/// Message types supported by Feishu.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Text,
    Post,
    Interactive,
    Image,
    File,
    Audio,
    Sticker,
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageType::Text => write!(f, "text"),
            MessageType::Post => write!(f, "post"),
            MessageType::Interactive => write!(f, "interactive"),
            MessageType::Image => write!(f, "image"),
            MessageType::File => write!(f, "file"),
            MessageType::Audio => write!(f, "audio"),
            MessageType::Sticker => write!(f, "sticker"),
        }
    }
}

impl std::str::FromStr for MessageType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(MessageType::Text),
            "post" => Ok(MessageType::Post),
            "interactive" => Ok(MessageType::Interactive),
            "image" => Ok(MessageType::Image),
            "file" => Ok(MessageType::File),
            "audio" => Ok(MessageType::Audio),
            "sticker" => Ok(MessageType::Sticker),
            _ => Err(format!("Unknown message type: {}", s)),
        }
    }
}

/// Send message request body.
#[derive(Debug, Clone, Serialize)]
pub struct SendMessageRequest {
    pub receive_id: String,
    #[serde(rename = "msg_type")]
    pub msg_type: MessageType,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// Reply message request body.
#[derive(Debug, Clone, Serialize)]
pub struct ReplyMessageRequest {
    #[serde(rename = "msg_type")]
    pub msg_type: MessageType,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// Send/reply message response.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    pub code: i32,
    pub msg: String,
    #[serde(default)]
    pub data: Option<MessageData>,
}

/// Message data in API response.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageData {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub root_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
}

/// Upload file response.
#[derive(Debug, Clone, Deserialize)]
pub struct UploadFileResponse {
    pub code: i32,
    pub msg: String,
    #[serde(default)]
    pub data: Option<UploadFileData>,
}

/// Upload file data.
#[derive(Debug, Clone, Deserialize)]
pub struct UploadFileData {
    #[serde(default)]
    pub file_key: Option<String>,
    #[serde(default)]
    pub image_key: Option<String>,
}

/// Add reaction request.
#[derive(Debug, Clone, Serialize)]
pub struct AddReactionRequest {
    pub reaction_type: ReactionType,
}

/// Reaction type (emoji).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionType {
    pub emoji_type: String,
}

/// Add reaction response.
#[derive(Debug, Clone, Deserialize)]
pub struct AddReactionResponse {
    pub code: i32,
    pub msg: String,
}

/// Get message response.
#[derive(Debug, Clone, Deserialize)]
pub struct GetMessageResponse {
    pub code: i32,
    pub msg: String,
    #[serde(default)]
    pub data: Option<GetMessageData>,
}

/// Message data from get message API.
#[derive(Debug, Clone, Deserialize)]
pub struct GetMessageData {
    #[serde(default)]
    pub items: Vec<MessageItem>,
}

/// Single message item.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageItem {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub root_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub msg_type: Option<String>,
    #[serde(default)]
    pub create_time: Option<String>,
    #[serde(default)]
    pub update_time: Option<String>,
    #[serde(default)]
    pub deleted: Option<bool>,
    #[serde(default)]
    pub updated: Option<bool>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub sender: Option<MessageSender>,
    #[serde(default)]
    pub body: Option<MessageBody>,
    #[serde(default)]
    pub mentions: Vec<Mention>,
}

/// Message sender info.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageSender {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "id_type")]
    pub id_type: Option<String>,
    #[serde(default)]
    pub sender_type: Option<String>,
    #[serde(default)]
    pub tenant_key: Option<String>,
}

/// Message body.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageBody {
    #[serde(default)]
    pub content: Option<String>,
}

/// Mention in a message.
#[derive(Debug, Clone, Deserialize)]
pub struct Mention {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub id: Option<MentionId>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tenant_key: Option<String>,
}

/// Mention ID.
#[derive(Debug, Clone, Deserialize)]
pub struct MentionId {
    #[serde(default)]
    pub open_id: Option<String>,
    #[serde(default)]
    pub union_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// Bot info response.
#[derive(Debug, Clone, Deserialize)]
pub struct BotInfoResponse {
    pub code: i32,
    pub msg: String,
    #[serde(default)]
    pub bot: Option<BotInfo>,
}

/// Bot info.
#[derive(Debug, Clone, Deserialize)]
pub struct BotInfo {
    #[serde(default)]
    pub open_id: Option<String>,
    #[serde(default)]
    pub union_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar: Option<BotAvatar>,
}

/// Bot avatar.
#[derive(Debug, Clone, Deserialize)]
pub struct BotAvatar {
    #[serde(default)]
    pub avatar_72: Option<String>,
    #[serde(default)]
    pub avatar_240: Option<String>,
    #[serde(default)]
    pub avatar_640: Option<String>,
}

/// WebSocket auth message.
#[derive(Debug, Clone, Serialize)]
pub struct WsAuthMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub header: WsAuthHeader,
}

/// WebSocket auth header.
#[derive(Debug, Clone, Serialize)]
pub struct WsAuthHeader {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
}

/// WebSocket auth message body.
#[derive(Debug, Clone, Serialize)]
pub struct WsAuthBody {
    pub authorization: WsAuthorization,
}

/// WebSocket authorization.
#[derive(Debug, Clone, Serialize)]
pub struct WsAuthorization {
    pub app_id: String,
    pub app_secret: String,
}

/// WebSocket message from server.
#[derive(Debug, Clone, Deserialize)]
pub struct WsMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default)]
    pub header: Option<WsHeader>,
    #[serde(default)]
    pub event: Option<serde_json::Value>,
    #[serde(default)]
    pub challenge: Option<String>,
}

/// WebSocket header.
#[derive(Debug, Clone, Deserialize)]
pub struct WsHeader {
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub create_time: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub tenant_key: Option<String>,
}

/// Receive message event.
#[derive(Debug, Clone, Deserialize)]
pub struct ReceiveMessageEvent {
    #[serde(default)]
    pub sender: Option<EventSender>,
    #[serde(default)]
    pub message: Option<EventMessage>,
    #[serde(default)]
    pub chat_id: Option<String>,
}

/// Event sender.
#[derive(Debug, Clone, Deserialize)]
pub struct EventSender {
    #[serde(default)]
    pub sender_id: Option<UserId>,
    #[serde(default)]
    pub sender_type: Option<String>,
    #[serde(default)]
    pub tenant_key: Option<String>,
}

/// User ID in event.
#[derive(Debug, Clone, Deserialize)]
pub struct UserId {
    #[serde(default)]
    pub open_id: Option<String>,
    #[serde(default)]
    pub union_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// Event message.
#[derive(Debug, Clone, Deserialize)]
pub struct EventMessage {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub root_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub msg_type: Option<String>,
    #[serde(default)]
    pub create_time: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub message_type: Option<String>,
    #[serde(default)]
    pub body: Option<EventMessageBody>,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    #[serde(default)]
    pub upper_message_id: Option<String>,
}

/// Event message body.
#[derive(Debug, Clone, Deserialize)]
pub struct EventMessageBody {
    #[serde(default)]
    pub content: Option<String>,
}

/// Text content for sending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContent {
    pub text: String,
}

/// Post content (rich text) for sending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostContent {
    #[serde(default)]
    pub post: PostBody,
}

/// Post body.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PostBody {
    #[serde(default)]
    pub zh_cn: Option<PostLocale>,
    #[serde(default)]
    pub en_us: Option<PostLocale>,
    #[serde(default)]
    pub ja_jp: Option<PostLocale>,
}

/// Post locale content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostLocale {
    pub title: String,
    pub content: Vec<Vec<PostElement>>,
}

/// Post element types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tag")]
pub enum PostElement {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "a")]
    Link { text: String, href: String },
    #[serde(rename = "at")]
    At {
        user_id: String,
        user_name: Option<String>,
    },
    #[serde(rename = "img")]
    Image { image_key: String },
    #[serde(rename = "media")]
    Media { file_key: String },
    #[serde(rename = "emotion")]
    Emotion { emoji_type: String },
}

/// Interactive card content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardContent {
    #[serde(default)]
    pub config: Option<CardConfig>,
    #[serde(default)]
    pub header: Option<CardHeader>,
    #[serde(default)]
    pub elements: Vec<CardElement>,
}

/// Card config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CardConfig {
    #[serde(default)]
    pub wide_screen_mode: bool,
    #[serde(default)]
    pub enable_forward: bool,
}

/// Card header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardHeader {
    pub title: CardTitle,
    #[serde(default)]
    pub template: Option<String>,
}

/// Card title.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardTitle {
    pub tag: String,
    pub content: String,
}

/// Card element.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tag")]
pub enum CardElement {
    #[serde(rename = "div")]
    Div {
        #[serde(default)]
        text: Option<CardText>,
        #[serde(default)]
        fields: Vec<CardField>,
    },
    #[serde(rename = "hr")]
    Hr,
    #[serde(rename = "markdown")]
    Markdown { content: String },
    #[serde(rename = "action")]
    Action { actions: Vec<CardAction> },
    #[serde(rename = "note")]
    Note { elements: Vec<CardNoteElement> },
    #[serde(rename = "img")]
    Image {
        img_key: String,
        #[serde(default)]
        alt: Option<CardText>,
    },
}

/// Card text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardText {
    pub tag: String,
    pub content: String,
    #[serde(default)]
    pub text_color: Option<String>,
}

/// Card field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardField {
    pub is_short: bool,
    pub text: CardText,
}

/// Card action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardAction {
    pub tag: String,
    pub text: CardText,
    pub url: Option<String>,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

/// Card note element.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tag")]
pub enum CardNoteElement {
    #[serde(rename = "plain_text")]
    PlainText { content: String },
    #[serde(rename = "img")]
    Image { img_key: String },
    #[serde(rename = "markdown")]
    Markdown { content: String },
}

/// User info response.
#[derive(Debug, Clone, Deserialize)]
pub struct UserInfoResponse {
    pub code: i32,
    pub msg: String,
    #[serde(default)]
    pub data: Option<UserInfoData>,
}

/// User info data.
#[derive(Debug, Clone, Deserialize)]
pub struct UserInfoData {
    #[serde(default)]
    pub user: Option<UserInfo>,
}

/// User info.
#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    #[serde(default)]
    pub open_id: Option<String>,
    #[serde(default)]
    pub union_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub mobile: Option<String>,
    #[serde(default)]
    pub avatar: Option<UserAvatar>,
}

/// User avatar.
#[derive(Debug, Clone, Deserialize)]
pub struct UserAvatar {
    #[serde(default)]
    pub avatar_72: Option<String>,
    #[serde(default)]
    pub avatar_240: Option<String>,
    #[serde(default)]
    pub avatar_640: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_type_display() {
        assert_eq!(MessageType::Text.to_string(), "text");
        assert_eq!(MessageType::Post.to_string(), "post");
        assert_eq!(MessageType::Interactive.to_string(), "interactive");
        assert_eq!(MessageType::Image.to_string(), "image");
    }

    #[test]
    fn message_type_from_str() {
        assert_eq!("text".parse::<MessageType>().unwrap(), MessageType::Text);
        assert_eq!("post".parse::<MessageType>().unwrap(), MessageType::Post);
        assert!("unknown".parse::<MessageType>().is_err());
    }

    #[test]
    fn send_message_request_serialization() {
        let req = SendMessageRequest {
            receive_id: "ou_123".to_string(),
            msg_type: MessageType::Text,
            content: r#"{"text":"hello"}"#.to_string(),
            uuid: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("receive_id"));
        assert!(json.contains("msg_type"));
    }

    #[test]
    fn text_content_serialization() {
        let content = TextContent {
            text: "Hello, world!".to_string(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("Hello, world!"));
    }

    #[test]
    fn ws_message_deserialization() {
        let json = r#"{"type": "connected", "header": {"app_id": "cli_123"}}"#;
        let msg: WsMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.msg_type, "connected");
    }

    #[test]
    fn card_content_serialization() {
        let card = CardContent {
            config: Some(CardConfig {
                wide_screen_mode: true,
                enable_forward: false,
            }),
            header: Some(CardHeader {
                title: CardTitle {
                    tag: "plain_text".to_string(),
                    content: "Test Card".to_string(),
                },
                template: Some("blue".to_string()),
            }),
            elements: vec![CardElement::Markdown {
                content: "**Hello**".to_string(),
            }],
        };
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("Test Card"));
        assert!(json.contains("Hello"));
    }
}

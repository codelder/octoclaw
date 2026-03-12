//! Session routing for Feishu group chats.

/// Group session scope determines how group chats are routed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupSessionScope {
    /// Entire group shares one session.
    Group,
    /// Each sender in the group gets their own session.
    GroupSender,
    /// Each topic thread gets its own session.
    GroupTopic,
}

impl GroupSessionScope {
    /// Parse from string.
    pub fn parse(s: &str) -> Self {
        match s {
            "group" => GroupSessionScope::Group,
            "group_sender" => GroupSessionScope::GroupSender,
            "group_topic" => GroupSessionScope::GroupTopic,
            _ => GroupSessionScope::GroupSender,
        }
    }
}

/// Compute the session key for routing messages.
pub fn compute_session_key(
    chat_id: &str,
    chat_type: &str,
    sender_id: Option<&str>,
    thread_id: Option<&str>,
    group_scope: &GroupSessionScope,
) -> String {
    match chat_type {
        "p2p" => {
            // DM: use sender's open_id
            format!("feishu:dm:{}", sender_id.unwrap_or("unknown"))
        }
        "group" | "chat" => {
            match group_scope {
                GroupSessionScope::Group => {
                    format!("feishu:group:{}", chat_id)
                }
                GroupSessionScope::GroupSender => {
                    format!(
                        "feishu:group:{}:sender:{}",
                        chat_id,
                        sender_id.unwrap_or("unknown")
                    )
                }
                GroupSessionScope::GroupTopic => {
                    if let Some(tid) = thread_id {
                        format!("feishu:group:{}:topic:{}", chat_id, tid)
                    } else {
                        // No thread, fall back to sender scope
                        format!(
                            "feishu:group:{}:sender:{}",
                            chat_id,
                            sender_id.unwrap_or("unknown")
                        )
                    }
                }
            }
        }
        _ => {
            // Unknown chat type, use generic
            format!("feishu:{}:{}", chat_type, chat_id)
        }
    }
}

/// Extract the receive_id type for sending messages.
pub fn receive_id_type(chat_type: &str) -> &'static str {
    match chat_type {
        "p2p" => "open_id",
        "group" | "chat" => "chat_id",
        _ => "chat_id",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_session_scope_parse() {
        assert_eq!(GroupSessionScope::parse("group"), GroupSessionScope::Group);
        assert_eq!(
            GroupSessionScope::parse("group_sender"),
            GroupSessionScope::GroupSender
        );
        assert_eq!(
            GroupSessionScope::parse("group_topic"),
            GroupSessionScope::GroupTopic
        );
        assert_eq!(
            GroupSessionScope::parse("unknown"),
            GroupSessionScope::GroupSender
        );
    }

    #[test]
    fn dm_session_key() {
        let key = compute_session_key(
            "oc_123",
            "p2p",
            Some("ou_user1"),
            None,
            &GroupSessionScope::GroupSender,
        );
        assert_eq!(key, "feishu:dm:ou_user1");
    }

    #[test]
    fn group_session_key() {
        let key = compute_session_key(
            "oc_group1",
            "group",
            Some("ou_user1"),
            None,
            &GroupSessionScope::Group,
        );
        assert_eq!(key, "feishu:group:oc_group1");
    }

    #[test]
    fn group_sender_session_key() {
        let key = compute_session_key(
            "oc_group1",
            "group",
            Some("ou_user1"),
            None,
            &GroupSessionScope::GroupSender,
        );
        assert_eq!(key, "feishu:group:oc_group1:sender:ou_user1");
    }

    #[test]
    fn group_topic_session_key() {
        let key = compute_session_key(
            "oc_group1",
            "group",
            Some("ou_user1"),
            Some("thread_123"),
            &GroupSessionScope::GroupTopic,
        );
        assert_eq!(key, "feishu:group:oc_group1:topic:thread_123");
    }

    #[test]
    fn group_topic_fallback_to_sender() {
        let key = compute_session_key(
            "oc_group1",
            "group",
            Some("ou_user1"),
            None,
            &GroupSessionScope::GroupTopic,
        );
        assert_eq!(key, "feishu:group:oc_group1:sender:ou_user1");
    }

    #[test]
    fn receive_id_type_mapping() {
        assert_eq!(receive_id_type("p2p"), "open_id");
        assert_eq!(receive_id_type("group"), "chat_id");
        assert_eq!(receive_id_type("chat"), "chat_id");
        assert_eq!(receive_id_type("unknown"), "chat_id");
    }
}

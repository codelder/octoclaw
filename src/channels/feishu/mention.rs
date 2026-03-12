//! @mention handling for Feishu messages.

use super::types::Mention;

/// Check if the bot is mentioned in the message.
pub fn is_bot_mentioned(mentions: &[Mention], bot_open_id: &str) -> bool {
    mentions.iter().any(|m| {
        if let Some(ref id) = m.id
            && let Some(ref open_id) = id.open_id
        {
            return open_id == bot_open_id;
        }
        false
    })
}

/// Extract mentioned user IDs from mentions.
pub fn extract_mentioned_user_ids(mentions: &[Mention]) -> Vec<String> {
    mentions
        .iter()
        .filter_map(|m| m.id.as_ref().and_then(|id| id.open_id.clone()))
        .collect()
}

/// Strip @mention tags from text content.
pub fn strip_mentions(text: &str) -> String {
    // Feishu mentions look like: <at user_id="ou_xxx">Name</at>
    let mut result = String::with_capacity(text.len());
    let mut in_mention = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            // Check if this is an <at tag
            let rest: String = chars.clone().take(3).collect();
            if rest.to_lowercase().starts_with("at ") {
                in_mention = true;
                // Skip until >
                for dc in chars.by_ref() {
                    if dc == '>' {
                        break;
                    }
                }
                continue;
            } else if rest.to_lowercase().starts_with("/at") {
                in_mention = false;
                // Skip until >
                for dc in chars.by_ref() {
                    if dc == '>' {
                        break;
                    }
                }
                continue;
            }
        }

        if !in_mention {
            result.push(c);
        }
    }

    result.trim().to_string()
}

/// Parse @mention XML tags and extract user info.
pub fn parse_mentions_from_xml(xml: &str) -> Vec<(String, String)> {
    // Returns (user_id, display_name) pairs
    let mut mentions = Vec::new();
    let mut chars = xml.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            let rest: String = chars.clone().take(3).collect();
            if rest.to_lowercase().starts_with("at ") {
                // Parse attributes
                let mut attrs = String::new();
                for dc in chars.by_ref() {
                    if dc == '>' {
                        break;
                    }
                    attrs.push(dc);
                }

                // Extract user_id
                let user_id = extract_attr(&attrs, "user_id");

                // Extract display name (content between <at> and </at>)
                let mut name = String::new();
                for dc in chars.by_ref() {
                    if dc == '<' {
                        break;
                    }
                    name.push(dc);
                }

                // Skip rest of closing tag
                for dc in chars.by_ref() {
                    if dc == '>' {
                        break;
                    }
                }

                if let Some(uid) = user_id {
                    mentions.push((uid, name.trim().to_string()));
                }
            }
        }
    }

    mentions
}

/// Extract an attribute value from an attribute string.
fn extract_attr(attrs: &str, name: &str) -> Option<String> {
    let pattern = format!("{}=", name);
    if let Some(start) = attrs.find(&pattern) {
        let after_eq = &attrs[start + pattern.len()..];
        let after_eq = after_eq.trim();
        if let Some(quote) = after_eq.chars().next()
            && (quote == '"' || quote == '\'')
        {
            let value_start = &after_eq[1..];
            if let Some(end) = value_start.find(quote) {
                return Some(value_start[..end].to_string());
            }
        }
    }
    None
}

/// Check if the message should be responded to based on mention requirements.
pub fn should_respond(
    chat_type: &str,
    require_mention: bool,
    is_mentioned: bool,
    _text: &str,
) -> bool {
    match chat_type {
        "p2p" => {
            // DMs always respond
            true
        }
        "group" | "chat" => {
            if require_mention {
                // Only respond if mentioned
                is_mentioned
            } else {
                // Respond to all messages
                true
            }
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{Mention, MentionId};
    use super::*;

    fn make_mention(open_id: &str) -> Mention {
        Mention {
            key: None,
            id: Some(MentionId {
                open_id: Some(open_id.to_string()),
                union_id: None,
                user_id: None,
            }),
            name: Some("Test User".to_string()),
            tenant_key: None,
        }
    }

    #[test]
    fn is_bot_mentioned_found() {
        let mentions = vec![make_mention("ou_user1"), make_mention("ou_bot123")];
        assert!(is_bot_mentioned(&mentions, "ou_bot123"));
    }

    #[test]
    fn is_bot_mentioned_not_found() {
        let mentions = vec![make_mention("ou_user1")];
        assert!(!is_bot_mentioned(&mentions, "ou_bot123"));
    }

    #[test]
    fn extract_mentioned_user_ids_works() {
        let mentions = vec![make_mention("ou_user1"), make_mention("ou_user2")];
        let ids = extract_mentioned_user_ids(&mentions);
        assert_eq!(ids, vec!["ou_user1", "ou_user2"]);
    }

    #[test]
    fn strip_mentions_works() {
        let text = "<at user_id=\"ou_bot\">Bot</at> hello world";
        assert_eq!(strip_mentions(text), "hello world");
    }

    #[test]
    fn parse_mentions_from_xml_works() {
        let xml = r#"<at user_id="ou_user1">Alice</at> hello <at user_id="ou_user2">Bob</at>"#;
        let mentions = parse_mentions_from_xml(xml);
        assert_eq!(mentions.len(), 2);
        assert_eq!(mentions[0].0, "ou_user1");
        assert_eq!(mentions[0].1, "Alice");
        assert_eq!(mentions[1].0, "ou_user2");
        assert_eq!(mentions[1].1, "Bob");
    }

    #[test]
    fn should_respond_dm() {
        assert!(should_respond("p2p", true, false, "hello"));
    }

    #[test]
    fn should_respond_group_with_mention() {
        assert!(should_respond("group", true, true, "hello"));
        assert!(!should_respond("group", true, false, "hello"));
    }

    #[test]
    fn should_respond_group_without_mention_requirement() {
        assert!(should_respond("group", false, false, "hello"));
    }
}

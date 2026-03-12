//! Feishu (Lark) channel implementation.
//!
//! This module provides integration with Feishu/Lark messaging platform,
//! supporting WebSocket long-polling for receiving messages and HTTP API
//! for sending messages, media, and reactions.

pub mod channel;
pub mod client;
pub mod media;
pub mod mention;
pub mod send;
pub mod session;
pub mod transport;
pub mod types;

pub use channel::FeishuChannel;
pub use client::FeishuClient;

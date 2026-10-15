//! Typed client for the Mattermost API v4 (REST) and its WebSocket event stream.
//!
//! The models deliberately cover only the fields the client uses and default
//! everything else, so they keep deserializing as servers add fields.

mod client;
mod error;
pub mod models;
pub mod ws;

pub use client::{Client, PostsQuery};
pub use error::{ApiError, Error};

//! Typed client for the Mattermost API v4 (REST) and its WebSocket event stream.
//!
//! The models deliberately cover only the fields the client uses and default
//! everything else, so they keep deserializing as servers add fields.

#[cfg(feature = "client")]
mod client;
mod error;
pub mod models;
#[cfg(feature = "client")]
pub mod ws;

#[cfg(feature = "client")]
pub use client::{Client, PostsQuery};
pub use error::ApiError;
#[cfg(feature = "client")]
pub use error::Error;

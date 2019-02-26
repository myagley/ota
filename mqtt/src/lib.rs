/*!
 * This crate contains an implementation of an MQTT client.
 */

#![deny(rust_2018_idioms, warnings)]
#![deny(clippy::all, clippy::pedantic)]
#![allow(
    clippy::default_trait_access,
    clippy::large_enum_variant,
    clippy::pub_enum_variant_names,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::stutter,
    clippy::too_many_arguments,
    clippy::use_self
)]

mod client;
pub use self::client::{
    Client, Error, Event, IoSource, PublishError, PublishHandle, ReceivedPublication,
    ShutdownError, ShutdownHandle, SubscriptionUpdate, UpdateSubscriptionError,
    UpdateSubscriptionHandle,
};

mod logging_framed;

pub mod proto;

//! Lumen's search engine daemon.
//!
//! The daemon owns `org.gnome.Lumen` on the session bus and answers on
//! `org.gnome.Lumen.Engine`. The extension is the only intended client; nothing here ties the
//! daemon's lifetime to any client.

pub mod dbus;
pub mod protocol;
pub mod providers;
pub mod ranking;
pub mod registry;
pub mod store;

pub use dbus::{serve, Config, Serving};
pub use registry::{Indexer, Registry};

//! Store seams. Ticket 11 replaces the in-memory clipboard history here with an encrypted,
//! on-disk one; nothing else in the daemon should have to change.

pub mod clipboard;

pub use clipboard::{ClipEntry, ClipboardStore};

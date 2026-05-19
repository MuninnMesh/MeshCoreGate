//! MeshCore packet and event data types shared by Muninn firmware crates.
//!
//! This crate is intentionally small and `no_std`: it holds fixed-capacity
//! data models and helpers for MeshCore messages, nodes, TX frames, and storage
//! records without owning radio, transport, or platform policy.

#![no_std]
#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]

/// MeshCore client-side packet helpers.
pub mod client;
/// MeshCore event and packet data models.
pub mod event;

pub use client::*;
pub use event::*;

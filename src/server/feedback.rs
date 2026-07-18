//! Finding feedback storage — compatibility re-exports.
//!
//! The implementation moved to the crate-level [`crate::feedback`] module so
//! the review pipeline (which must not depend on the server) shares the same
//! fingerprint algorithm and storage format. This module simply re-exports
//! everything to keep the existing `server::feedback` API paths working; the
//! HTTP endpoints live in `super::api::feedback`.

pub use crate::feedback::*;

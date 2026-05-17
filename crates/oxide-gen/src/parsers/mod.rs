//! Parsers convert each spec format into a common [`crate::ir::ApiSpec`].
//!
//! The parsers in this module deliberately implement only the parts of each
//! spec format that the generator can usefully emit. Unsupported constructs
//! (polymorphic schemas, recursive types, custom scalars, …) are degraded to
//! `serde_json::Value` or a documented `String` alias rather than aborting.

pub mod graphql;
pub mod naming;
pub mod openapi;
pub mod proto;

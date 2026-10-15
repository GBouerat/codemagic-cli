//! Strongly typed data models representing Codemagic CI/CD API schemas.

pub mod app;
pub mod artifact;
pub mod build;
pub mod common;
pub mod user;

pub use app::*;
pub use artifact::*;
pub use build::*;
pub use common::*;
pub use user::*;

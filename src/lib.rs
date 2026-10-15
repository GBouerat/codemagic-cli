//! # codemagic
//!
//! A fast, ergonomic, cross-platform CLI tool for the [Codemagic](https://codemagic.io) CI/CD service,
//! written in Rust.
//!
//! ## Overview
//!
//! This crate provides both library modules and the binary application for interacting
//! with Codemagic CI/CD:
//!
//! - [`cli`]: Command-line parsing, argument structures, and subcommands.
//! - [`client`]: Multi-tier REST API client supporting Codemagic v3 and Classic APIs.
//! - [`config`]: Persistent filesystem configuration and 3-tier credentials resolution.
//! - [`models`]: Strongly typed, resilient Serde models for Codemagic resources.
//! - [`output`]: Dual-mode presentation engine for formatted tables and raw JSON.

pub mod cli;
pub mod client;
pub mod config;
pub mod models;
pub mod output;

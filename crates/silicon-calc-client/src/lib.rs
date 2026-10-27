//! # silicon-calc-client
//!
//! A typed async client for the [Silicon Calc](https://calc.teamofsilicons.com) service: sheets and folders,
//! sharing and transfers, tabs and cells, edits, live events, browser command result commits, imports and downloads,
//! copies and merges, versions, comments and search. Carbons and Silicons use the same API; the `calc` command
//! line is built on this crate.
//!
//! ```no_run
//! # async fn demo() -> silicon_calc_client::Result<()> {
//! use silicon_calc_client::{Client, Edit, EditRequest, Line, NewSheet, TabRef};
//! let client = Client::new("https://calc-api.teamofsilicons.com")?.with_token("<access token for calc>");
//! let sheet = client.create_sheet(&NewSheet { title: Some("Q3 revenue".into()), folder_id: None }).await?;
//! let line = Line::main(&sheet.id);
//! let rows = vec![vec!["region".into(), "amount".into()], vec!["EU".into(), "1200.5".into()]];
//! client
//!     .edit(&line, &EditRequest { base_version: Some(sheet.version), edits: vec![Edit::set_block(1, "A1", rows)], client_id: None })
//!     .await?;
//! let cells = client.cells(&line, &TabRef::Id(1), Some("A1:B2"), None).await?;
//! for cell in &cells.cells {
//!     println!("{} = {}", cell.a, cell.t);
//! }
//! # Ok(()) }
//! ```
//!
//! **Sign-in.** Every request carries a Silicon Accounts access token issued to the app `calc`. Pass one with
//! [`Client::with_token`], or give a [`TokenSource`] that keeps a session ([`Client::with_token_source`]): the
//! client asks it for a token before each request and, after a 401, for a fresh one once, then tries again.
//! Public sheets can be read without a token.
//!
//! **Errors** are typed ([`Error`]) and carry the service's `status`, `code`, `message`, `hint`, `details` and
//! `Retry-After`. A request answered `slow_down` (429) is tried again after the wait the service asks for.
//!
//! **Files** stream: [`Upload::from_path`] sends a file from disk without reading it into memory, and
//! [`Download`] hands back the body as a stream (or saves it to a file).
//!
//! **Live events** ([`Client::events`]) arrive as typed [`Event`]s and reconnect by themselves from the last
//! version seen.
//!
//! This crate records no telemetry of any kind.

#![warn(missing_docs)]

pub mod a1;
mod client;
mod error;
mod events;
mod types;

pub use client::{
    Client, ClientBuilder, DEFAULT_API_URL, Download, Line, StaticToken, TokenSource, Upload, VERSION,
    content_disposition_filename,
};
pub use error::{Error, Result};
pub use events::{Event, EventOptions, EventStream, RawEvent, SseParser};
pub use types::*;

//! Sends Silicon Calc's notifications to Silicon Ting.
//!
//! Contract (the service's notification worker calls exactly this):
//! `Publisher::new(Config)?.send(&body).await -> Result<Accepted, Error>`, where `body` is one ting
//! (`{"type", "for", "key", "data", "metadata"}`, docs/API.md). Each send carries Calc's App verification proof for
//! Ting (receiving app `ting`, scope `tings.send`), issued by Silicon Accounts with Calc's app credentials and
//! cached until shortly before it expires. Built by the e2e/Ting work item (docs/BUILDING.md).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Config {
    /// Silicon Ting's origin (`https://ting.teamofsilicons.com`); the WebSocket is `{ting_url}/v1/ws?protocol=v1`.
    pub ting_url: String,
    /// Where Calc reaches the Silicon Accounts API, to issue its proof.
    pub accounts_api_url: String,
    pub app_id: String,
    pub app_secret: String,
    /// The receiving app of the proof (`ting`).
    pub ting_app_id: String,
    pub allow_http: bool,
}

/// Ting stored the ting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Accepted {
    pub id: String,
    pub created_at: String,
    pub silent: bool,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("Silicon Ting could not be reached: {0}")]
    Transport(String),
    #[error("Silicon Ting refused the ting: {code}")]
    Rejected { code: String },
    #[error("Silicon Accounts could not issue Calc's proof for Ting: {0}")]
    Authority(String),
    #[error("Silicon Ting answered something unexpected: {0}")]
    Protocol(String),
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// Whether sending the same ting again later may succeed.
    pub fn retryable(&self) -> bool {
        !matches!(self, Self::Invalid(_)) && !matches!(self, Self::Rejected { code } if code == "invalid_ting" || code == "unknown_recipient")
    }
}

pub struct Publisher {
    _cfg: Config,
}

impl Publisher {
    pub fn new(cfg: Config) -> Result<Self, Error> {
        Ok(Self { _cfg: cfg })
    }

    /// Sends one ting and waits for Ting to accept it.
    pub async fn send(&self, _body: &serde_json::Value) -> Result<Accepted, Error> {
        Err(Error::Transport("the Ting publisher is not built yet".into()))
    }
}

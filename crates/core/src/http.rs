//! The HTTP client every registry lookup and download uses.
use crate::{Error, Result};

/// A client builder with depsmith's user agent and a `timeout` in seconds.
pub(crate) fn builder(timeout: u64) -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .user_agent("depsmith/0.1")
}

/// Build `builder` into a client.
pub(crate) fn build(
    builder: reqwest::blocking::ClientBuilder,
) -> Result<reqwest::blocking::Client> {
    builder
        .build()
        .map_err(|_| Error::Operation("cannot construct HTTP client".into()))
}

/// A client with depsmith's user agent and a `timeout` in seconds.
pub(crate) fn client(timeout: u64) -> Result<reqwest::blocking::Client> {
    build(builder(timeout))
}

use std::{fmt::Debug, io::Read, time::Duration};

use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};

use crate::peer::{deserialize_peers, Peer};

#[derive(Debug, serde_derive::Serialize)]
pub enum Event {
    Started,
    Completed,
    Stopped,
    Empty,
}

#[derive(Debug, serde_derive::Serialize)]
pub struct Params {
    #[serde(skip)]
    pub info_hash: [u8; 20],
    pub peer_id: String,
    pub ip: Option<String>,
    pub port: u16,
    pub uploaded: usize,
    pub downloaded: usize,
    pub left: usize,
    pub event: Option<Event>,
    pub compact: i32,
}

#[derive(Debug, serde_derive::Deserialize)]
pub struct Response {
    #[serde(rename = "failure reason")]
    pub failure_reason: Option<String>,
    pub interval: Option<u64>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_peers")]
    pub peers: Vec<Peer>,
}

#[derive(Debug)]
pub enum Error {
    QueryError(String),
    HTTPError(reqwest::Error),
    BencodeError(serde_bencode::Error),
    URLEncodeError(serde_urlencoded::ser::Error),
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        return Error::HTTPError(e);
    }
}

impl From<serde_bencode::Error> for Error {
    fn from(e: serde_bencode::Error) -> Self {
        Error::BencodeError(e)
    }
}

impl From<serde_urlencoded::ser::Error> for Error {
    fn from(e: serde_urlencoded::ser::Error) -> Self {
        Error::URLEncodeError(e)
    }
}

const FRAGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'~')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.');

pub async fn announce(url: String, param: Params) -> Result<Response, Error> {
    let query = serde_urlencoded::to_string(&param)?;
    let res = percent_encode(&param.info_hash, FRAGMENT);
    let tracker_url = format!("{}?info_hash={}&{}", url, res.to_string(), query);

    let response = reqwest::get(&tracker_url).await?.bytes().await.map_err(|e| Error::HTTPError(e))?;

    match serde_bencode::from_bytes::<Response>(&response) {
        Ok(res) => {
            if let Some(failure_reason) = res.failure_reason {
                Err(Error::QueryError(failure_reason))
            } else {
                Ok(res)
            }
        }
        Err(e) => Err(Error::BencodeError(e))
    }
}

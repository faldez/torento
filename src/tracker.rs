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
    HTTPError(String),
    BencodeError(serde_bencode::Error),
    URLEncodeError(serde_urlencoded::ser::Error),
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(_code, res) => {
                let mut buf = vec![];
                let reader = res.into_reader();
                for byte in reader.bytes() {
                    buf.push(byte.unwrap())
                }

                match serde_bencode::from_bytes::<Response>(&buf).map_err(|e| Error::BencodeError(e)) {
                    Ok(parsed) => {
                        Error::QueryError(parsed.failure_reason.unwrap())
                    }
                    Err(e) => {
                        e
                    }
                }
                
            }
            ureq::Error::Transport(t) => Error::HTTPError(t.to_string()),
        }
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

pub fn announce(url: String, param: Params) -> Result<Response, Error> {
    let query = serde_urlencoded::to_string(&param)?;
    let res = percent_encode(&param.info_hash, FRAGMENT);
    let tracker_url = format!("{}?info_hash={}&{}", url, res.to_string(), query);

    let result = ureq::get(&tracker_url).timeout(Duration::new(120, 0)).call()?;

    let mut buf = vec![];
    let reader = result.into_reader();
    for byte in reader.bytes() {
        buf.push(byte.unwrap())
    }

    serde_bencode::from_bytes::<Response>(&buf).map_err(|e| Error::BencodeError(e))
}

use std::{fmt::Debug, net::{SocketAddr, IpAddr, Ipv4Addr}, sync::Arc};
use std::convert::TryInto;
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserializer, Serializer, de::{Visitor, SeqAccess}, Serialize, Deserialize};

use crate::torrent::TorrentContext;

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Event {
    Started,
    Completed,
    Stopped,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug,serde::Deserialize)]
pub struct Response {
    #[serde(rename = "failure reason")]
    pub failure_reason: Option<String>,
    pub interval: Option<u64>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_peers")]
    pub peers: Vec<SocketAddr>,
}

pub fn deserialize_peers<'de, D>(deserializer: D) -> Result<Vec<SocketAddr>, D::Error>
where
    D: Deserializer<'de>,
{
    struct PeersVisitor;

    impl<'de> Visitor<'de> for PeersVisitor {
        type Value = Vec<SocketAddr>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("byte array")
        }

        fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let len = v.len();
            let mut peers = vec![];
            if len == 0 {
                return Ok(peers);
            }
            for i in (0..len).step_by(6) {
                let port = u16::from_be_bytes(v[i + 4..i + 6].try_into().unwrap());

                peers.push(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(v[i], v[i + 1], v[i + 2], v[i + 3])),
                    port,
                ))
            }

            Ok(peers)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            #[derive(Deserialize)]
            struct PeerDict {
                peer_id: [u8; 20],
                ip: IpAddr,
                port: u16,
            };

            let mut peers = vec![];
            while let Some(PeerDict { peer_id, ip, port }) = seq.next_element()? {
                let address = SocketAddr::new(ip, port);
                peers.push(address)
            }

            Ok(peers)
        }
    }

    deserializer.deserialize_any(PeersVisitor)
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

pub const FRAGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'~')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.');

pub async fn announce(ctx: Arc<TorrentContext>) -> Result<Response, Error> {
    let query = serde_urlencoded::to_string(&ctx.params)?;
    let encoded = percent_encode(&ctx.metainfo.info_hash, FRAGMENT);
    
    let tracker_url = format!("{}?info_hash={}&{}", ctx.metainfo.announce, encoded.to_string(), query);
    info!("announce to {}", tracker_url);

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

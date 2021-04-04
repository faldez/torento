use anyhow::{anyhow, Result};
use serde::{
    de::{Error, SeqAccess, Visitor},
    Deserializer,
};
use std::{borrow::BorrowMut, convert::TryInto, mem::size_of, net::{IpAddr, Ipv4Addr}};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[derive(Debug)]
pub enum Message {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(Vec<u8>),
    Request(u32, u32, u32),
    Piece(u32, u32, Vec<u8>),
    Cancel(u32, u32, u32),
    KeepAlive,
    Invalid,
}

impl From<Vec<u8>> for Message {
    fn from(bytes: Vec<u8>) -> Self {
        if bytes.len() == 0 {
            return Message::KeepAlive;
        }

        match bytes[0] {
            0 => Message::Choke,
            1 => Message::Unchoke,
            2 => Message::Interested,
            3 => Message::NotInterested,
            4 => {
                let mut payload: [u8; 4] = [0; 4];
                payload.copy_from_slice(&bytes[1..]);
                let index = u32::from_be_bytes(payload);
                Message::Have(index)
            },
            5 => {
                let payload = bytes[1..].to_vec();
                Message::Bitfield(payload)
            }
            6 => {
                let mut payload: [u8; 4] = [0; 4];
                payload.copy_from_slice(&bytes[1..5]);
                let index = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[5..9]);
                let begin = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[9..13]);
                let length = u32::from_be_bytes(payload);

                Message::Request(index, begin, length)
            },
            7 => {
                let mut payload: [u8; 4] = [0; 4];
                payload.copy_from_slice(&bytes[1..5]);
                let index = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[5..9]);
                let begin = u32::from_be_bytes(payload);

                let piece = bytes[9..].to_vec();
                
                Message::Piece(index, begin, piece)
            },
            8 => {
                let mut payload: [u8; 4] = [0; 4];
                payload.copy_from_slice(&bytes[1..5]);
                let index = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[5..9]);
                let begin = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[9..13]);
                let length = u32::from_be_bytes(payload);

                Message::Cancel(index, begin, length)
            },
            _ => Message::Invalid,
        }
    }
}

impl Into<Vec<u8>> for Message {
    fn into(self) -> Vec<u8> {
        let mut message = vec![];
        match self {
            Message::Choke => {
                message.push(0);
            }
            Message::Unchoke => {
                message.push(1);
            }
            Message::Interested => {
                message.push(2);
            }
            Message::NotInterested => {
                message.push(3);
            }
            Message::Have(index) => {
                message.push(4);
                message.extend(&index.to_be_bytes());
            }
            Message::Bitfield(index) => {
                message.push(5);
                message.extend(&index);
            }
            Message::Request(index, begin, length) => {
                message.push(6);
                message.extend(&index.to_be_bytes());
                message.extend(&begin.to_be_bytes());
                message.extend(&length.to_be_bytes());
            }
            Message::Piece(index, begin, piece) => {
                message.push(7);
                message.extend(&index.to_be_bytes());
                message.extend(&begin.to_be_bytes());
                message.extend(&piece);
            }
            Message::Cancel(index, begin, length) => {
                message.push(8);
                message.append(&mut index.to_be_bytes().to_vec());
                message.append(&mut begin.to_be_bytes().to_vec());
                message.append(&mut length.to_be_bytes().to_vec());
            }
            Message::KeepAlive => {}
            Message::Invalid => {}
        }
        return message;
    }
}

#[derive(Debug, Clone, serde_derive::Deserialize)]
pub struct Peer {
    #[serde(default)]
    #[serde(rename = "peer id")]
    pub peer_id: [u8; 20],
    pub ip: IpAddr,
    pub port: u16,
}

pub fn deserialize_peers<'de, D>(deserializer: D) -> Result<Vec<Peer>, D::Error>
where
    D: Deserializer<'de>,
{
    struct PeersVisitor;

    impl<'de> Visitor<'de> for PeersVisitor {
        type Value = Vec<Peer>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("byte array")
        }

        fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
        where
            E: Error,
        {
            let len = v.len();
            let mut peers = vec![];
            if len == 0 {
                return Ok(peers);
            }
            for i in (0..len).step_by(6) {
                let port = u16::from_be_bytes(v[i + 4..i + 6].try_into().unwrap());

                peers.push(Peer {
                    peer_id: [0; 20],
                    ip: IpAddr::V4(Ipv4Addr::new(v[i], v[i + 1], v[i + 2], v[i + 3])),
                    port,
                })
            }

            Ok(peers)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut peers = vec![];
            while let Some(Peer { peer_id, ip, port }) = seq.next_element()? {
                peers.push(Peer { peer_id, ip, port })
            }

            Ok(peers)
        }
    }

    deserializer.deserialize_any(PeersVisitor)
}

pub struct Session {
    conn: TcpStream,
    peer: Peer,
    peer_id: String,
}

impl Session {
    pub async fn connect(peer_id: String, peer: Peer) -> Result<Self> {
        info!("connecting to {:?}:{}", peer.ip, peer.port);
        let conn = match TcpStream::connect((peer.ip, peer.port)).await {
            Ok(conn) => conn,
            Err(e) => return Err(anyhow!(e)),
        };

        Ok(Self {
            peer,
            conn,
            peer_id,
        })
    }

    pub async fn handshake(&mut self, info_hash: &[u8]) -> Result<(), anyhow::Error> {
        let mut buf = vec![];
        buf.extend_from_slice(&[19; 1]);
        buf.extend_from_slice(b"BitTorrent protocol");
        buf.extend_from_slice(&[0; 8]);
        buf.extend_from_slice(&info_hash);
        buf.extend_from_slice(&self.peer_id.as_bytes());
        match self.conn.write_all(&buf).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to send handsake {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        let mut reply = [0; 68];
        match self.conn.read_exact(&mut reply).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to read handsake {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        info!("{:?}", reply);

        if reply[0] != 19 {
            return Err(anyhow::anyhow!("wrong first byte"));
        }

        if String::from_utf8(reply[1..20].to_vec()).unwrap() != "BitTorrent protocol".to_string() {
            return Err(anyhow::anyhow!("wrong protocol"));
        }

        Ok(())
    }

    pub async fn read_message(&mut self) -> Result<Message> {
        let mut len_prefix = [0; 4];
        match self.conn.read_exact(&mut len_prefix).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to read message {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        let length = u32::from_be_bytes(len_prefix);

        info!("len: {}", length);

        let mut message = vec![0; length as usize];
        match self.conn.read_exact(&mut message).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to read message {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        Ok(message.into())
    }

    pub async fn send_message(&mut self, message_type: Message) -> Result<()> {
        let payload: Vec<u8> = message_type.into();
        info!("send message {:?}", payload);
        match self.conn.write_all(&payload).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to send message {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        Ok(())
    }
}

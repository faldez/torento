use anyhow::{anyhow, Result};
use async_channel::{Receiver, Sender};
use bitvec::{bitvec, order::Msb0, prelude::BitVec};
use serde::{
    de::{Error, SeqAccess, Visitor},
    Deserializer,
};
use std::{
    borrow::BorrowMut,
    collections::HashMap,
    convert::TryInto,
    mem::size_of,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio::{net::TcpListener, task::JoinHandle};

use crate::{
    metainfo::{self, Metainfo},
    torrent::{Torrent, TorrentContext},
};

#[derive(Debug)]
pub enum Message {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(BitVec<Msb0, u8>),
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
            }
            5 => {
                let payload = bytes[1..].to_vec();
                Message::Bitfield(BitVec::from_vec(payload))
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
            }
            7 => {
                let mut payload: [u8; 4] = [0; 4];
                payload.copy_from_slice(&bytes[1..5]);
                let index = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[5..9]);
                let begin = u32::from_be_bytes(payload);

                let piece = bytes[9..].to_vec();

                Message::Piece(index, begin, piece)
            }
            8 => {
                let mut payload: [u8; 4] = [0; 4];
                payload.copy_from_slice(&bytes[1..5]);
                let index = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[5..9]);
                let begin = u32::from_be_bytes(payload);

                payload.copy_from_slice(&bytes[9..13]);
                let length = u32::from_be_bytes(payload);

                Message::Cancel(index, begin, length)
            }
            _ => Message::Invalid,
        }
    }
}

impl Into<Vec<u8>> for Message {
    fn into(self) -> Vec<u8> {
        let mut message = vec![];
        match self {
            Message::Choke => {
                message.extend(&(1 as u32).to_be_bytes());
                message.push(0);
            }
            Message::Unchoke => {
                message.extend(&(1 as u32).to_be_bytes());
                message.push(1);
            }
            Message::Interested => {
                message.extend(&(1 as u32).to_be_bytes());
                message.push(2);
            }
            Message::NotInterested => {
                message.extend(&(1 as u32).to_be_bytes());
                message.push(3);
            }
            Message::Have(index) => {
                message.extend(&(5 as u32).to_be_bytes());
                message.push(4);
                message.extend(&index.to_be_bytes());
            }
            Message::Bitfield(index) => {
                message.extend(&(1 + index.len()).to_be_bytes());
                message.push(5);
                message.extend(index.as_raw_slice());
            }
            Message::Request(index, begin, length) => {
                message.extend(&(13 as u32).to_be_bytes());
                message.push(6);
                message.extend(&index.to_be_bytes());
                message.extend(&begin.to_be_bytes());
                message.extend(&length.to_be_bytes());
            }
            Message::Piece(index, begin, piece) => {
                message.extend(&(1 + piece.len()).to_be_bytes());
                message.push(7);
                message.extend(&index.to_be_bytes());
                message.extend(&begin.to_be_bytes());
                message.extend(&piece);
            }
            Message::Cancel(index, begin, length) => {
                message.extend(&(13 as u32).to_be_bytes());
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

pub struct Peer {
    pub peer_id: [u8; 20],
    pub address: SocketAddr,
    ctx: Arc<TorrentContext>,
    conn: Option<TcpStream>,
    peer_tx: Sender<Message>,
    cmd_rx: Receiver<Message>,
    bitfield: BitVec<Msb0, u8>,
}

impl Peer {
    pub fn new(
        ctx: Arc<TorrentContext>,
        address: SocketAddr,
        peer_tx: Sender<Message>,
        cmd_rx: Receiver<Message>,
    ) -> Self {
        let total_piece = ctx.piece_counter.total_pieces as usize;
        Self {
            peer_id: [0; 20],
            address,
            conn: None,
            peer_tx,
            cmd_rx,
            ctx,
            bitfield: bitvec![Msb0, u8; 0; total_piece],
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            if self.conn.is_none() {
                let conn = match TcpStream::connect(self.address).await {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("connect {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        continue;
                    }
                };
                info!("connected to {:?}", self.address);

                self.conn = Some(conn);

                let info_hash = self.ctx.metainfo.info_hash;
                if let Err(e) = self.handshake(&info_hash).await {
                    error!("handsake {}", e);
                    self.conn = None;
                    tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                    continue;
                }

                match self.send_message(Message::Interested).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("interested {}", e);
                        self.conn = None;
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        continue;
                    }
                };
            }

            if let Ok(msg) = self.cmd_rx.try_recv() {
                info!("{:?}", msg);
                match self.send_message(msg).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("{}", e);
                        self.conn = None;
                    }
                };
            }

            let msg = match self.read_message().await {
                Ok(msg) => msg,
                Err(e) => {
                    error!("{}", e);
                    self.conn = None;
                    continue;
                }
            };

            match &msg {
                Message::Bitfield(index) => {
                    self.bitfield.copy_from_bitslice(index);
                }
                _ => {}
            }
            self.peer_tx.send(msg).await.unwrap();

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    pub async fn handshake(&mut self, info_hash: &[u8]) -> Result<(), anyhow::Error> {
        let mut buf = vec![];
        buf.push(19);
        buf.extend(b"BitTorrent protocol");
        buf.extend(&[0; 8]);
        buf.extend(info_hash);
        buf.extend(&[0; 20]);
        match self.conn.as_mut().unwrap().write_all(&buf).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to send handsake {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        let mut reply = [0; 68];
        match self.conn.as_mut().unwrap().read_exact(&mut reply).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to read handsake {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        if reply[0] != 19 {
            return Err(anyhow::anyhow!("wrong first byte: {:?}", reply));
        }

        if String::from_utf8(reply[1..20].to_vec()).unwrap() != "BitTorrent protocol".to_string() {
            return Err(anyhow::anyhow!("wrong protocol"));
        }

        Ok(())
    }

    pub async fn read_message(&mut self) -> Result<Message> {
        let mut len_prefix = [0; 4];
        match self.conn.as_mut().unwrap().read(&mut len_prefix).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to read length message {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        let length = u32::from_be_bytes(len_prefix);

        let mut message = vec![0; length as usize];
        match self.conn.as_mut().unwrap().read_exact(&mut message).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to read payload message {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        Ok(message.into())
    }

    pub async fn send_message(&mut self, message_type: Message) -> Result<()> {
        let payload: Vec<u8> = message_type.into();
        match self.conn.as_mut().unwrap().write_all(&payload).await {
            Ok(_) => {}
            Err(e) => {
                error!("failed to send message {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        Ok(())
    }
}

pub struct Session {
    handle: JoinHandle<()>,
}

impl Session {
    pub fn new(mut peer: Peer) -> Self {
        let handle = tokio::spawn(async move {
            let _ = peer.run().await;
        });

        Self { handle }
    }
}

pub struct PeerManager {
    pub ctx: Arc<TorrentContext>,
    pub peer_tx: Sender<Message>,
    pub cmd_rx: Receiver<Message>,
    pub peers: HashMap<SocketAddr, Session>,
}

impl PeerManager {
    pub fn new(peer_tx: Sender<Message>, ctx: Arc<TorrentContext>) -> (Self, Sender<Message>) {
        let (cmd_tx, cmd_rx) = async_channel::unbounded();
        (
            Self {
                ctx,
                peer_tx,
                cmd_rx,
                peers: HashMap::new(),
            },
            cmd_tx,
        )
    }

    pub fn connect_peers(&mut self, mut address: Vec<SocketAddr>) {
        for addr in address.drain(..) {
            let peer = Peer::new(
                self.ctx.clone(),
                addr,
                self.peer_tx.clone(),
                self.cmd_rx.clone(),
            );
            self.peers.insert(addr, Session::new(peer));
        }
    }
}

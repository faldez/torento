use anyhow::{anyhow, Result};
use async_channel::{Receiver, Sender};
use bitvec::{bitvec, order::Msb0, prelude::BitVec, view::BitView};
use linked_hash_set::LinkedHashSet;
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
    piece::{Block, Piece},
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

pub enum PeerCommand {
    Message(Message),
    Shutdown,
}

pub struct Peer {
    pub peer_id: [u8; 20],
    pub address: SocketAddr,
    ctx: Arc<TorrentContext>,
    conn: Option<TcpStream>,
    peer_tx: Sender<Message>,
    cmd_rx: Receiver<PeerCommand>,
    choke: bool,
    outgoing_requests: LinkedHashSet<Block>,
}

impl Peer {
    pub fn new(
        ctx: Arc<TorrentContext>,
        address: SocketAddr,
        peer_tx: Sender<Message>,
        cmd_rx: Receiver<PeerCommand>,
    ) -> Self {
        Self {
            peer_id: [0; 20],
            address,
            conn: None,
            peer_tx,
            cmd_rx,
            ctx,
            choke: false,
            outgoing_requests: LinkedHashSet::new(),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            // if there is no connection, try connecting
            if self.conn.is_none() {
                let conn = match TcpStream::connect(self.address).await {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("connect {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        return Err(anyhow!(e));
                    }
                };
                info!("connected to {:?}", self.address);

                self.conn = Some(conn);

                let info_hash = self.ctx.metainfo.info_hash;

                // if there is error while handshaking, disconnect and try again
                if let Err(e) = self.handshake(&info_hash).await {
                    error!("handsake {}", e);
                    self.conn = None;
                    tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                    return Err(anyhow!(e));
                }

                // if there is error while send message Interested, disconnect and try again
                match self.send_message(Message::Interested).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("interested {}", e);
                        self.conn = None;
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        return Err(anyhow!(e));
                    }
                };
            }

            if let Ok(cmd) = self.cmd_rx.try_recv() {
                match cmd {
                    PeerCommand::Message(msg) => match msg {
                        Message::Request(index, begin, length) => {
                            if !self.choke {
                                self.outgoing_requests.insert(Block {
                                    index,
                                    begin,
                                    length,
                                });
                            }
                        }
                        _ => {}
                    },
                    PeerCommand::Shutdown => {
                        return Ok(());
                    }
                }
            }

            // pop outgoing request from front
            // send request message if there is request queue
            if let Some(req) = self.outgoing_requests.pop_front() {
                // check if block downloaded, don't request if downloaded
                if !self.ctx.piece_counter.read().await.downloaded[req.index as usize]
                    [req.begin as usize / 16384]
                {
                    // send request message
                    info!("{:?} > {:?}", self.address, req);
                    match self
                        .send_message(Message::Request(req.index, req.begin, req.length))
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            // disconnect if send message failed
                            error!("{}", e);
                            self.conn = None;
                            return Err(anyhow!(e));
                        }
                    }
                }
            }

            let msg = match self.read_message().await {
                Ok(msg) => msg,
                Err(e) => {
                    // disconnect from peer if read_message return error
                    // so we can retry connecting
                    error!("{}", e);
                    self.conn = None;
                    tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                    return Err(anyhow!(e));
                }
            };

            match &msg {
                Message::Choke => {
                    self.choke = true;
                }
                Message::Unchoke => {
                    self.choke = false;
                }
                _ => {
                    self.peer_tx.send(msg).await.unwrap();
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    pub async fn handshake(&mut self, info_hash: &[u8]) -> Result<(), anyhow::Error> {
        if let Some(conn) = self.conn.as_mut() {
            let mut buf = vec![];
            buf.push(19);
            buf.extend(b"BitTorrent protocol");
            buf.extend(&[0; 8]);
            buf.extend(info_hash);
            buf.extend(&[0; 20]);
            match conn.write_all(&buf).await {
                Ok(_) => {}
                Err(e) => {
                    error!("failed to send handsake {}", e);
                    self.conn = None;
                    return Err(anyhow::anyhow!(e));
                }
            }

            let mut reply = [0; 68];
            let mut i = 0;
            loop {
                // Wait for the socket to be readable
                conn.readable().await?;

                // Try to read data, this may still fail with `WouldBlock`
                // if the readiness event is a false positive.
                match conn.try_read(&mut reply[i..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        i += n;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(e) => {
                        self.conn = None;
                        return Err(e.into());
                    }
                }
            }

            if reply[0] != 19 {
                self.conn = None;
                return Err(anyhow::anyhow!(
                    "{} > wrong first byte: {:?}",
                    self.address,
                    reply
                ));
            }

            if String::from_utf8(reply[1..20].to_vec()).unwrap()
                != "BitTorrent protocol".to_string()
            {
                self.conn = None;
                return Err(anyhow::anyhow!("wrong protocol"));
            }

            Ok(())
        } else {
            Err(anyhow!("{} > Connection not exist", self.address))
        }
    }

    pub async fn read_message(&mut self) -> Result<Message> {
        if let Some(conn) = self.conn.as_mut() {
            let mut len_prefix = [0; 4];
            match conn.read(&mut len_prefix).await {
                Ok(_) => {}
                Err(e) => {
                    error!("failed to read length message {}", e);
                    return Err(anyhow::anyhow!(e));
                }
            }

            let length = u32::from_be_bytes(len_prefix);

            let mut message = vec![0; length as usize];
            match conn.read_exact(&mut message).await {
                Ok(_) => {}
                Err(e) => {
                    error!("failed to read payload message {}", e);
                    return Err(anyhow::anyhow!(e));
                }
            }

            Ok(message.into())
        } else {
            Err(anyhow!("{} > Connection not exist", self.address))
        }
    }

    pub async fn send_message(&mut self, message_type: Message) -> Result<()> {
        if let Some(conn) = self.conn.as_mut() {
            let payload: Vec<u8> = message_type.into();
            match conn.write_all(&payload).await {
                Ok(_) => {}
                Err(e) => {
                    error!("failed to send message {}", e);
                    return Err(anyhow::anyhow!(e));
                }
            }

            Ok(())
        } else {
            Err(anyhow!("Connection not exist"))
        }
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
    pub cmd_rx: Receiver<PeerCommand>,
    pub peers: HashMap<SocketAddr, Session>,
}

impl PeerManager {
    pub fn new(peer_tx: Sender<Message>, ctx: Arc<TorrentContext>) -> (Self, Sender<PeerCommand>) {
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
        for addr in address.drain(..10) {
            let peer = Peer::new(
                self.ctx.clone(),
                addr,
                self.peer_tx.clone(),
                self.cmd_rx.clone(),
            );
            self.peers.insert(addr, Session::new(peer));
        }
    }

    pub fn shutdown(&mut self) {
        self.peers.clear();
    }
}
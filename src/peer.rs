use anyhow::{anyhow, Result};
use async_channel::{Receiver, Sender};
use bitvec::{order::Msb0, prelude::BitVec};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    task::JoinHandle,
};

use crate::torrent::TorrentContext;

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

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Message::Choke => {
                write!(f, "Choke")
            }
            Message::Unchoke => {
                write!(f, "Unchoke")
            }
            Message::Interested => {
                write!(f, "Interested")
            }
            Message::NotInterested => {
                write!(f, "NotInterested")
            }
            Message::Have(index) => {
                write!(f, "Have({})", index)
            }
            Message::Bitfield(_) => {
                write!(f, "Bitfield")
            }
            Message::Request(index, begin, length) => {
                write!(f, "Request({}, {}, {})", index, begin, length)
            }
            Message::Piece(index, begin, piece) => {
                write!(f, "Piece({}, {}, [{}])", index, begin, piece.len())
            }
            Message::Cancel(index, begin, length) => {
                write!(f, "Cancel({}, {}, {})", index, begin, length)
            }
            Message::KeepAlive => {
                write!(f, "KeepAlive")
            }
            Message::Invalid => {
                write!(f, "Invalid")
            }
        }
    }
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

pub enum AlertMessage {
    Received(SocketAddr, Message),
    Connecting(SocketAddr),
    Connected(SocketAddr),
    Quit(SocketAddr),
}

pub enum PeerCommand {
    Send(Message),
    Shutdown,
}

pub struct Peer {
    pub peer_id: [u8; 20],
    pub address: SocketAddr,
    ctx: Arc<TorrentContext>,
    conn: Option<TcpStream>,
    peer_tx: Sender<AlertMessage>,
    cmd_rx: Receiver<PeerCommand>,
    bitfield: BitVec<Msb0, u8>,
    choke: bool,
    ready: bool,
}

impl Peer {
    pub fn new(
        ctx: Arc<TorrentContext>,
        address: SocketAddr,
        peer_tx: Sender<AlertMessage>,
        cmd_rx: Receiver<PeerCommand>,
    ) -> Self {
        Self {
            peer_id: [0; 20],
            address,
            conn: None,
            peer_tx,
            cmd_rx,
            ctx,
            bitfield: BitVec::new(),
            choke: false,
            ready: true,
        }
    }

    pub async fn initiate_outgoing_connection(&mut self) -> Result<()> {
        self.conn = match TcpStream::connect(self.address).await {
            Ok(conn) => Some(conn),
            Err(e) => {
                return Err(anyhow!("{} > connection: {}", self.address, e));
            }
        };

        let info_hash = self.ctx.metainfo.info_hash;

        // if there is error while handshaking, disconnect and try again
        if let Err(e) = self.handshake(&info_hash).await {
            return Err(anyhow!("{} > handshake: {}", self.address, e));
        }

        // if there is error while send message Interested, disconnect and try again
        if let Err(e) = self.send_message(Message::Interested).await {
            return Err(anyhow!("{} > interested: {}", self.address, e));
        };

        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        self.peer_tx
            .send(AlertMessage::Connecting(self.address))
            .await
            .unwrap();

        // if there is no connection, try connecting
        if let Err(e) = self.initiate_outgoing_connection().await {
            self.peer_tx
                .send(AlertMessage::Quit(self.address))
                .await
                .unwrap();
            return Err(e);
        }

        self.peer_tx
            .send(AlertMessage::Connected(self.address))
            .await
            .unwrap();

        loop {
            if let Ok(cmd) = self.cmd_rx.try_recv() {
                match cmd {
                    PeerCommand::Send(_) => {}
                    PeerCommand::Shutdown => {
                        return Ok(());
                    }
                }
            }

            let msg = match self.read_message().await {
                Ok(msg) => msg,
                Err(e) => {
                    // quit from peer if read_message return error
                    return Err(e);
                }
            };

            if let Some(msg) = msg {
                info!("{} > Receive: {}", self.address, msg);

                match &msg {
                    Message::Choke => {
                        self.choke = true;
                    }
                    Message::Unchoke => {
                        self.choke = false;
                        self.ready = true;
                    }
                    Message::Bitfield(index) => {
                        self.bitfield.clone_from(index);
                    }
                    Message::Have(index) => {
                        self.bitfield.set(*index as usize, true);
                    }
                    _ => {
                        self.peer_tx
                            .send(AlertMessage::Received(self.address, msg))
                            .await
                            .unwrap();
                        self.ready = true;
                    }
                }
            }

            // pop outgoing request from front
            // send request message if there is request queue
            if !self.choke && self.ready {
                let block = {
                    let mut outgoing_requests = self.ctx.outgoing_requests.write().await;
                    let req = outgoing_requests.front();
                    if let Some(req) = req {
                        if let Some(val) = self.bitfield.get(req.index as usize).as_deref() {
                            if *val {
                                outgoing_requests.pop_front()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                if let Some(req) = block {
                    if !self.ctx.piece_counter.read().await.downloaded[req.index as usize]
                        [req.begin as usize / 16384]
                    {
                        // send request message
                        info!("{:?} > Send {:?}", self.address, req);
                        if let Err(e) = self
                            .send_message(Message::Request(req.index, req.begin, req.length))
                            .await
                        {
                            return Err(anyhow!(e));
                        }
                        self.ready = false;
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
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
            match conn.read_exact(&mut reply).await {
                Ok(_) => {}
                Err(e) => {
                    error!("failed to read handshake message {}", e);
                    return Err(anyhow::anyhow!(e));
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

    pub async fn read_message(&mut self) -> Result<Option<Message>> {
        if let Some(conn) = self.conn.as_mut() {
            let mut len_prefix = [0; 4];
            match tokio::time::timeout(std::time::Duration::from_secs(1), conn.read_exact(&mut len_prefix)).await {
                Ok(res) => {
                    match res {
                        Ok(_) => {}
                        Err(e) => {
                            error!("failed to read length message {}", e);
                            return Err(anyhow::anyhow!(e));
                        }
                    }
                }
                Err(_) => {
                    return Ok(None);
                }
            }

            let length = u32::from_be_bytes(len_prefix);

            let mut message = vec![0; length as usize];
            match tokio::time::timeout(std::time::Duration::from_secs(1), conn.read_exact(&mut message)).await {
                Ok(res) => {
                    match res {
                        Ok(_) => {}
                        Err(e) => {
                            error!("failed to read message {}", e);
                            return Err(anyhow::anyhow!(e));
                        }
                    }
                }
                Err(_) => {
                    return Ok(None);
                }
            }

            Ok(Some(message.into()))
        } else {
            Err(anyhow!("{} > Connection not exist", self.address))
        }
    }

    pub async fn send_message(&mut self, message: Message) -> Result<()> {
        if let Some(conn) = self.conn.as_mut() {
            let payload: Vec<u8> = message.into();
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
    pub peer_tx: Sender<AlertMessage>,
    pub cmd_rx: Receiver<PeerCommand>,
    pub available_peers: Vec<SocketAddr>,
    pub peers: HashMap<SocketAddr, Session>,
}

impl PeerManager {
    pub fn new(
        peer_tx: Sender<AlertMessage>,
        ctx: Arc<TorrentContext>,
    ) -> (Self, Sender<PeerCommand>) {
        let (cmd_tx, cmd_rx) = async_channel::unbounded();
        (
            Self {
                ctx,
                peer_tx,
                cmd_rx,
                available_peers: Vec::new(),
                peers: HashMap::new(),
            },
            cmd_tx,
        )
    }

    pub fn set_available_peers(&mut self, address: Vec<SocketAddr>) {
        self.available_peers = address;
    }

    pub fn connect_peers(&mut self) {
        let max_connection = if self.available_peers.len() > 5 {
            5
        } else {
            self.available_peers.len()
        };

        if self.peers.len() > max_connection {
            return;
        }

        for addr in self.available_peers.drain(..max_connection) {
            let peer = Peer::new(
                self.ctx.clone(),
                addr,
                self.peer_tx.clone(),
                self.cmd_rx.clone(),
            );
            self.peers.insert(addr, Session::new(peer));
        }
    }

    pub fn shutdown_peer(&mut self, peer: SocketAddr) {
        self.peers.remove(&peer);
    }
}

use std::{sync::Arc, time::Duration};

use crate::{metainfo::Metainfo, peer::{Message, PeerCommand, PeerManager}, piece::{Piece, PieceCounter}, tracker::{self, Params}, writer};
use anyhow::{anyhow, Result};
use async_channel::{Receiver, Sender};
use bitvec::{
    bitvec,
    prelude::{BitVec, Msb0},
};
use tokio::{sync::{RwLock, mpsc::{UnboundedReceiver, UnboundedSender}}, task::JoinHandle};

pub struct TorrentContext {
    pub metainfo: Metainfo,
    pub params: Params,
    pub piece_counter: RwLock<PieceCounter>,
}

pub struct Torrent {
    pub ctx: Arc<TorrentContext>,
    pub peer_rx: Receiver<Message>,
    pub cmd_tx: Sender<PeerCommand>,
    pub peer_manager: PeerManager,
    pub writer_handle: JoinHandle<()>,
    pub writer_tx: UnboundedSender<Piece>,
}

impl Torrent {
    pub fn new(metainfo: Metainfo) -> Self {
        let (peer_tx, peer_rx) = async_channel::unbounded();

        let length = if let Some(left) = &metainfo.info.length {
            *left
        } else if let Some(files) = &metainfo.info.files {
            files.iter().fold(0, |acc, file| acc + file.length)
        } else {
            0
        };

        let params = tracker::Params {
            info_hash: metainfo.info_hash,
            peer_id: "00000000000000000000".to_string(),
            ip: None,
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: length,
            event: None,
            compact: 1,
        };

        let total_pieces =
            ((length + (metainfo.info.piece_length - 1)) / metainfo.info.piece_length) as u32;
        let total_pieces = ((total_pieces + 8 - 1) / 8) * 8;
                
        let piece_counter = RwLock::new(PieceCounter {
            total_pieces,
            piece_index: bitvec![Msb0, u8; 0; total_pieces as usize],
            downloaded: vec![vec![false; metainfo.info.piece_length / 16384]; total_pieces as usize],
        });

        let ctx = Arc::new(TorrentContext { metainfo, params, piece_counter });

        let (peer_manager, cmd_tx) = PeerManager::new(peer_tx, ctx.clone());

        let (writer_handle, writer_tx) = writer::start(ctx.clone());

        Self {
            ctx,
            peer_rx,
            cmd_tx,
            peer_manager,
            writer_handle,
            writer_tx,
        }
    }
    pub async fn run(&mut self) {
        let mut resp = tracker::announce(self.ctx.clone()).await.unwrap();
        info!("announce interval in {}s", resp.interval.unwrap_or(100));

        self.peer_manager.connect_peers(resp.peers);

        let n = self.ctx.metainfo.info.piece_length / 16384;
        let mut blocks = vec![false; n];

        let mut now = std::time::Instant::now();
        let mut now_announce = std::time::Instant::now();

        let mut piece_index = 0_u32;

        loop {
            piece_index = match self.ctx.piece_counter.read().await.piece_index.iter_zeros().next() {
                Some(index) => {
                    if piece_index != index as u32 {
                        blocks = vec![false; n];
                    }

                    index as u32
                },
                None => {
                    info!("all piece downlaoded");
                    self.peer_manager.shutdown();
                    return;
                }
            };

            if let Ok(msg) = self.peer_rx.recv().await {
                match msg {
                    Message::Choke => {}
                    Message::Unchoke => {}
                    Message::Interested => {}
                    Message::NotInterested => {}
                    Message::Have(_) => {}
                    Message::Bitfield(_) => {}
                    Message::Request(_, _, _) => {}
                    Message::Piece(index, begin, piece) => {
                        if !self.ctx.piece_counter.read().await.downloaded[index as usize][begin as usize / 16384] && index == piece_index {
                            self.writer_tx.send(Piece {
                                index: index as usize,
                                begin: begin as usize,
                                piece,
                            }).unwrap();
                        }
                    }
                    Message::Cancel(_, _, _) => {}
                    Message::KeepAlive => {}
                    Message::Invalid => {}
                }
            }

            if std::time::Instant::now().saturating_duration_since(now)
                > std::time::Duration::from_secs(15)
            {
                for i in 0..n {
                    if self.ctx.piece_counter.read().await.downloaded[piece_index as usize][i] {
                        continue;
                    }

                    self.cmd_tx
                        .send(PeerCommand::Message(Message::Request(piece_index, (i * 16384) as u32, 16384)))
                        .await
                        .unwrap();
                }

                now = std::time::Instant::now();
            }

            if std::time::Instant::now().saturating_duration_since(now_announce)
                > std::time::Duration::from_secs(resp.interval.unwrap_or(60))
            {
                resp = tracker::announce(self.ctx.clone()).await.unwrap();

                self.peer_manager.connect_peers(resp.peers);

                now_announce = std::time::Instant::now();
            }
        }
    }
}

pub fn start(metainfo: Metainfo) -> Result<JoinHandle<()>> {
    let mut torrent = Torrent::new(metainfo);

    let handle = tokio::spawn(async move { torrent.run().await });

    Ok(handle)
}

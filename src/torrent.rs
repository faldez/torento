use std::{sync::Arc, time::Duration};

use crate::{
    metainfo::Metainfo,
    peer::{self, Message, Peer, PeerManager, Session},
    piece::{Piece, PieceCounter},
    tracker::{self, Params},
    writer::WriterHandle,
};
use anyhow::{anyhow, Result};
use async_channel::{Receiver, Sender};
use bitvec::{bitvec, prelude::Msb0};
use tokio::fs::metadata;
use tokio::task::JoinHandle;
use tracker::Event;

pub struct TorrentContext {
    pub metainfo: Metainfo,
    pub piece_counter: PieceCounter,
    pub params: Params,
}

pub struct Torrent {
    pub ctx: Arc<TorrentContext>,
    pub peer_rx: Receiver<Message>,
    pub cmd_tx: Sender<Message>,
    pub peer_manager: PeerManager,
    pub writer: WriterHandle,
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
        info!("total pieces: {}", total_pieces);
        let piece_counter = PieceCounter {
            total_pieces,
            downloaded: bitvec![Msb0, u8; 0; total_pieces as usize],
        };

        let ctx = Arc::new(TorrentContext {
            metainfo,
            piece_counter,
            params,
        });

        let (peer_manager, cmd_tx) = PeerManager::new(peer_tx, ctx.clone());

        let writer = WriterHandle::start(ctx.clone());

        Self {
            ctx,
            peer_rx,
            cmd_tx,
            peer_manager,
            writer,
        }
    }
    pub async fn run(&mut self) {
        let resp = tracker::announce(self.ctx.clone()).await.unwrap();

        self.peer_manager.connect_peers(resp.peers);

        let n = self.ctx.metainfo.info.piece_length / 16384;
        let mut blocks = vec![false; n];

        let mut now = std::time::Instant::now();
        
        loop {
            let piece_index = match self.ctx.piece_counter.downloaded.iter_zeros().next() {
                Some(index) => {
                    index as u32
                }
                None => {
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
                        if !blocks[begin as usize / 16384] {
                            let piece = Piece {
                                index: index as usize,
                                begin: begin as usize,
                                piece,
                            };
                            self.writer.send(piece);
                            blocks[begin as usize / 16384] = true;
                        }
                    }
                    Message::Cancel(_, _, _) => {}
                    Message::KeepAlive => {}
                    Message::Invalid => {}
                }
            }

            if std::time::Instant::now().saturating_duration_since(now)
                > std::time::Duration::from_secs(1)
            {
                for i in 0..n {
                    if blocks[i] {
                        continue;
                    }

                    self.cmd_tx
                        .send(Message::Request(piece_index, (i * 16384) as u32, 16384))
                        .await
                        .unwrap();
                }

                now = std::time::Instant::now();
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

pub fn start(metainfo: Metainfo) -> Result<JoinHandle<()>> {
    let mut torrent = Torrent::new(metainfo);

    let handle = tokio::spawn(async move { torrent.run().await });

    Ok(handle)
}

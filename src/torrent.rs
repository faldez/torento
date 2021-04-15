use std::{sync::Arc, time::Duration};

use crate::{
    metainfo::Metainfo,
    peer::{AlertMessage, Message, PeerCommand, PeerManager},
    piece::{Block, Piece, PieceCounter},
    tracker::{self, Params},
    writer,
};
use anyhow::{anyhow, Result};
use async_channel::{Receiver, Sender};
use bitvec::{
    bitvec,
    prelude::{BitVec, Msb0},
};
use linked_hash_set::LinkedHashSet;
use tokio::{
    sync::{
        mpsc::{UnboundedReceiver, UnboundedSender},
        RwLock,
    },
    task::JoinHandle,
};

pub struct TorrentContext {
    pub metainfo: Metainfo,
    pub params: Params,
    pub piece_counter: RwLock<PieceCounter>,
    pub outgoing_requests: RwLock<LinkedHashSet<Block>>,
}

pub struct Torrent {
    pub ctx: Arc<TorrentContext>,
    pub peer_rx: Receiver<AlertMessage>,
    pub cmd_tx: Sender<PeerCommand>,
    pub peer_manager: PeerManager,
    pub writer_handle: JoinHandle<()>,
    pub writer_tx: UnboundedSender<Piece>,
}

impl Torrent {
    pub fn new(metainfo: Metainfo) -> Self {
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

        let total_pieces = (length / metainfo.info.piece_length) as u32;
        info!("total pieces: {}", total_pieces);

        let piece_counter = RwLock::new(PieceCounter {
            total_pieces,
            piece_index: bitvec![Msb0, u8; 0; total_pieces as usize],
            downloaded: vec![
                vec![false; metainfo.info.piece_length / 16384];
                total_pieces as usize
            ],
        });

        let ctx = Arc::new(TorrentContext {
            metainfo,
            params,
            piece_counter,
            outgoing_requests: RwLock::new(LinkedHashSet::new()),
        });

        let (peer_tx, peer_rx) = async_channel::unbounded();
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
        let mut resp = {
            if let Ok(resp) = tracker::announce(self.ctx.clone()).await {
                resp
            } else {
                tracker::announce(self.ctx.clone()).await.unwrap()
            }
        };

        info!("announce interval in {}s", resp.interval.unwrap_or(100));

        self.peer_manager.set_available_peers(resp.peers);

        let n = self.ctx.metainfo.info.piece_length / 16384;

        let mut now_announce = std::time::Instant::now();

        loop {
            self.peer_manager.connect_peers();

            match self
                .ctx
                .piece_counter
                .read()
                .await
                .piece_index
                .iter_zeros()
                .next()
            {
                Some(_) => {}
                None => {
                    info!("all piece downlaoded");
                    return;
                }
            };

            if let Ok(msg) = self.peer_rx.try_recv() {
                match msg {
                    AlertMessage::Received(peer, m) => match m {
                        Message::Choke => {
                            info!("{} > Choke", peer);
                        }
                        Message::Unchoke => {
                            info!("{} > Unchoke", peer);
                        }
                        Message::Interested => {}
                        Message::NotInterested => {}
                        Message::Have(_) => {}
                        Message::Bitfield(_) => {}
                        Message::Request(_, _, _) => {}
                        Message::Piece(index, begin, piece) => {
                            if !self.ctx.piece_counter.read().await.downloaded[index as usize]
                                [begin as usize / 16384]
                            {
                                self.writer_tx
                                    .send(Piece {
                                        index: index as usize,
                                        begin: begin as usize,
                                        piece,
                                    })
                                    .unwrap();
                            }
                        }
                        Message::Cancel(_, _, _) => {}
                        Message::KeepAlive => {}
                        Message::Invalid => {}
                    },

                    AlertMessage::Connecting(_) => {}
                    AlertMessage::Connected(_) => {}
                    AlertMessage::Quit(addr) => {
                        error!("{} > quitting...", addr);
                        self.peer_manager.shutdown_peer(addr);
                    }
                }
            }

            if self.ctx.outgoing_requests.read().await.len() < 10 {
                let mut requesting_blocks_len = 0_u32;
                let mut mutable_outgoing_requests = self.ctx.outgoing_requests.write().await;
                for (index, blocks) in self
                    .ctx
                    .piece_counter
                    .read()
                    .await
                    .downloaded
                    .iter()
                    .enumerate()
                {
                    for (offset, downloaded) in blocks.iter().enumerate() {
                        if !downloaded {
                            mutable_outgoing_requests.insert_if_absent(Block {
                                index: index as u32,
                                begin: offset as u32 * 16384,
                                length: 16384,
                            });
                            requesting_blocks_len += 1;
                        }
                        if requesting_blocks_len >= 10 {
                            break;
                        }
                    }
                    if requesting_blocks_len >= 10 {
                        break;
                    }
                }
            }

            if std::time::Instant::now().saturating_duration_since(now_announce)
                > std::time::Duration::from_secs(resp.interval.unwrap_or(60))
            {
                resp = {
                    if let Ok(resp) = tracker::announce(self.ctx.clone()).await {
                        resp
                    } else {
                        tracker::announce(self.ctx.clone()).await.unwrap()
                    }
                };

                self.peer_manager.set_available_peers(resp.peers);

                now_announce = std::time::Instant::now();
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

pub fn start(metainfo: Metainfo) -> Result<JoinHandle<()>> {
    let mut torrent = Torrent::new(metainfo);

    let handle = tokio::spawn(async move { torrent.run().await });

    Ok(handle)
}

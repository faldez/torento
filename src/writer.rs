use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::{fs::File, io::{AsyncWriteExt, AsyncSeekExt}};
use std::{io::SeekFrom, sync::Arc};
use tokio::task::JoinHandle;
use anyhow::Result;
use std::io::Write;
use crate::{piece::Piece, torrent::TorrentContext};

pub struct Writer {
    ctx: Arc<TorrentContext>,
    writer_rx: UnboundedReceiver<Piece>,
    files: Vec<File>,
    buffer: Vec<u8>,
    piece: Vec<bool>
}

impl Writer {
    pub fn new(ctx: Arc<TorrentContext>, writer_rx: UnboundedReceiver<Piece>) -> Self {
        let files = vec![];

        let buffer = vec![0; ctx.metainfo.info.piece_length];
        let piece = vec![false; ctx.metainfo.info.piece_length / 16384];

        Self { ctx, writer_rx , files, buffer, piece }
    }

    pub async fn run(&mut self) {
        if let Some(available_files) = self.ctx.metainfo.info.files.as_ref() {
            std::fs::create_dir(&self.ctx.metainfo.info.name).unwrap();
            for file in available_files.iter() {
                let mut path = std::path::PathBuf::from(file.path[0].clone());
                for p in file.path.clone().drain(1..) {
                    path.push(&p);
                }
                let f = File::create(path).await.unwrap();
                f.set_len(file.length as u64).await.unwrap();
                self.files.push(f);
            }
        } else {
            let file = File::create(&self.ctx.metainfo.info.name).await.unwrap();
            file.set_len(self.ctx.metainfo.info.length.unwrap() as u64).await.unwrap();
            self.files.push(file);
        }

        loop {
            if let Some(piece) = self.writer_rx.recv().await {
                info!("write {} bytes at {}", piece.piece.len(), piece.begin);
                self.buffer.splice(piece.begin..piece.begin + piece.piece.len(), piece.piece);
                self.piece[piece.begin / 16384] = true;

                let mut complete = true;
                for p in self.piece.iter() {
                    if !p {
                        complete = false;
                        break;
                    }
                }

                if complete {
                    self.files[0].seek(SeekFrom::Start((piece.index * 16384) as u64)).await.unwrap();
                    self.files[0].write_all(&self.buffer).await.unwrap();
                    info!("write to file {} bytes", self.buffer.len());
                    self.buffer = vec![0; self.ctx.metainfo.info.piece_length];
                }
            }
        }
    }
}

pub struct WriterHandle {
    handle: JoinHandle<()>,
    writer_tx: UnboundedSender<Piece>,
}

impl WriterHandle {
    pub fn start(ctx: Arc<TorrentContext>) -> Self {
        let (writer_tx, writer_rx) = tokio::sync::mpsc::unbounded_channel();
    
        let mut writer = Writer::new(ctx, writer_rx);
    
        let handle = tokio::spawn(async move {
            writer.run().await;
        });
    
        WriterHandle{
            handle,
            writer_tx
        }
    }

    pub fn send(&self, piece: Piece) {
        self.writer_tx.send(piece).unwrap();
    }
}

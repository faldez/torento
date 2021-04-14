use crate::{piece::Piece, torrent::TorrentContext};
use anyhow::Result;
use async_channel::Sender;
use bitvec::{order::Msb0, prelude::BitVec};
use std::io::Write;
use std::{io::SeekFrom, sync::Arc};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::{
    fs::File,
    io::{AsyncSeekExt, AsyncWriteExt},
};

pub struct Writer {
    ctx: Arc<TorrentContext>,
    writer_rx: UnboundedReceiver<Piece>,
    files: Vec<File>,
    buffer: Vec<u8>,
    blocks: Vec<bool>,
}

impl Writer {
    pub fn new(ctx: Arc<TorrentContext>, writer_rx: UnboundedReceiver<Piece>) -> Self {
        let files = vec![];

        let buffer = vec![0; ctx.metainfo.info.piece_length];
        let blocks = vec![false; ctx.metainfo.info.piece_length / 16384];

        Self {
            ctx,
            writer_rx,
            files,
            buffer,
            blocks,
        }
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
            file.set_len(self.ctx.metainfo.info.length.unwrap() as u64)
                .await
                .unwrap();
            self.files.push(file);
        }

        loop {
            if let Some(piece) = self.writer_rx.recv().await {
                self.buffer
                    .splice(piece.begin..piece.begin + piece.piece.len(), piece.piece);
                self.ctx.piece_counter.write().await.downloaded[piece.index][piece.begin / 16384] =
                    true;

                let offset = (piece.index * self.ctx.metainfo.info.piece_length) + piece.begin;
                self.files[0]
                    .seek(SeekFrom::Start(offset as u64))
                    .await
                    .unwrap();
                self.files[0].write_all(&self.buffer).await.unwrap();

                info!("write piece {} to file at {}", piece.index, offset);
                if self.ctx.piece_counter.read().await.downloaded[piece.index]
                    .iter()
                    .find(|p| **p == false)
                    .is_none()
                {
                    self.ctx
                        .piece_counter
                        .write()
                        .await
                        .piece_index
                        .set(piece.index, true);
                    info!("piece {}/{} done", piece.index, self.ctx.piece_counter.read().await.total_pieces);
                }
            }
        }
    }
}

pub fn start(ctx: Arc<TorrentContext>) -> (JoinHandle<()>, UnboundedSender<Piece>) {
    let (writer_tx, writer_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut writer = Writer::new(ctx, writer_rx);

    let handle = tokio::spawn(async move {
        writer.run().await;
    });

    (handle, writer_tx)
}

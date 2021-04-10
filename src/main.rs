extern crate pretty_env_logger;
#[macro_use]
extern crate log;
use anyhow::{anyhow, Result};
use futures::future::join_all;
use rand::{distributions::Alphanumeric, Rng};
use tokio::{fs::File, io::AsyncReadExt, task::JoinHandle};
use torrent::Torrent;
use std::sync::Arc;

mod metainfo;
mod peer;
mod torrent;
mod tracker;

use crate::metainfo::Metainfo;
use crate::tracker::announce;

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();

    let mut buffer = vec![];
    let mut file = File::open(
        "/Users/fadhlika/Downloads/Bottom-Tier Character Tomozaki [Yen Press] [LuCaZ].torrent",
    )
    .await
    .unwrap();
    if let Err(e) = file.read_to_end(&mut buffer).await {
        return Err(anyhow!(e));
    }

    let metainfo = Metainfo::from_bytes(&buffer);

    info!("{:?}", metainfo.announce);
    info!("{:?}", metainfo.info.name);
    info!("{:?}", metainfo.info.piece_length);
    info!("{:?}", metainfo.info.files);
    info!("{:?}", metainfo.info_hash);

    let left = if let Some(left) = &metainfo.info.length {
        *left
    } else if let Some(files) = &metainfo.info.files {
        files.iter().fold(0, |acc, file| acc + file.length)
    } else {
        0
    };

    let peer_id: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .collect();

    let params = tracker::Params {
        info_hash: metainfo.info_hash,
        peer_id: peer_id.clone(),
        ip: None,
        port: 6881,
        uploaded: 0,
        downloaded: 0,
        left,
        event: None,
        compact: 1,
    };

    let mut resp = announce(&metainfo.announce, &params).await.unwrap();

    let torrent = Arc::new(Torrent {
        metainfo,
        params
    });


    let mut handles: Vec<JoinHandle<()>> = vec![];
    for peer in resp.peers.drain(..) {
        let torrent = torrent.clone();
        handles.push(tokio::spawn(async move {
            let mut session = match peer.connect(torrent).await {
                Ok(session) => session,
                Err(_) => {
                    return;
                }
            };

            session.run().await;
        }));
    }

    join_all(handles).await;

    Ok(())
}

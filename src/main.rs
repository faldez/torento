extern crate pretty_env_logger;
#[macro_use]
extern crate log;
use anyhow::{anyhow, Result};
use futures::future::join_all;
use rand::{distributions::Alphanumeric, Rng};
use tokio::{fs::File, io::AsyncReadExt, task::JoinHandle};

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

    let torrent = Metainfo::from_bytes(&buffer);

    info!("{:?}", torrent.announce);
    info!("{:?}", torrent.info.name);
    info!("{:?}", torrent.info.piece_length);
    info!("{:?}", torrent.info.files);
    info!("{:?}", torrent.info_hash);

    let left = if let Some(left) = torrent.info.length {
        left
    } else if let Some(files) = torrent.info.files {
        files.iter().fold(0, |acc, file| acc + file.length)
    } else {
        0
    };

    let peer_id: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .collect();
    let announce_param = tracker::Params {
        info_hash: torrent.info_hash,
        peer_id: peer_id.clone(),
        ip: None,
        port: 6881,
        uploaded: 0,
        downloaded: 0,
        left,
        event: None,
        compact: 1,
    };
    let resp = announce(torrent.announce, announce_param).await.unwrap();

    let mut handles: Vec<JoinHandle<()>> = vec![];
    for peer in resp.peers.iter() {
        let p = peer.clone();
        let id = peer_id.clone();
        let info_hash = torrent.info_hash.clone();

        handles.push(tokio::spawn(async move {
            let mut session = match peer::Session::connect(id, p.clone()).await {
                Ok(session) => session,
                Err(_) => {
                    return;
                }
            };

            if session.handshake(&info_hash).await.is_err() {
                return;
            }

            match session.send_message(peer::Message::Interested).await {
                Ok(_) => {}
                Err(_) => {
                    return;
                }
            };

            loop {
                let msg = match session.read_message().await {
                    Ok(msg) => msg,
                    Err(_) => {
                        return;
                    }
                };

                info!("{:?}", msg);
            }
        }));
    }

    join_all(handles).await;

    Ok(())
}

extern crate pretty_env_logger;
#[macro_use]
extern crate log;
use anyhow::{anyhow, Result};
use tokio::{fs::File, io::AsyncReadExt};

mod metainfo;
mod peer;
mod torrent;
mod tracker;
mod piece;
mod writer;

use crate::metainfo::Metainfo;

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();

    let mut buffer = vec![];
    let mut file = File::open(
        "C:\\Users\\fadhlika\\Downloads\\ubuntu-20.04.2-live-server-amd64.iso.torrent",
        // "C:\\Users\\fadhlika\\Downloads\\[SubsPlease] 86 - Eighty Six - 01 (1080p) [1B13598F].mkv.torrent"
    )
    .await
    .unwrap();
    
    if let Err(e) = file.read_to_end(&mut buffer).await {
        return Err(anyhow!(e));
    }

    let metainfo = Metainfo::from_bytes(&buffer);
    let torrent_handle = torrent::start(metainfo).unwrap();

    let _ = tokio::join!(torrent_handle);

    tokio::signal::ctrl_c().await?;

    Ok(())
}

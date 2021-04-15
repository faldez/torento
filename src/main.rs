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

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        return Err(anyhow!("Path to torrent file is necessary"));
    }

    let mut buffer = vec![];
    let mut file = File::open(&args[1])
    .await
    .unwrap();
    
    if let Err(e) = file.read_to_end(&mut buffer).await {
        return Err(anyhow!(e));
    }

    let metainfo = metainfo::Metainfo::from_bytes(&buffer);
    let torrent_handle = torrent::start(metainfo).unwrap();

    let _ = tokio::join!(torrent_handle);

    tokio::signal::ctrl_c().await?;

    Ok(())
}

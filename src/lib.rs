extern crate pretty_env_logger;
#[macro_use]
extern crate log;

mod metainfo;
mod peer;
mod piece;
mod torrent;
mod tracker;
mod writer;

#[cfg(test)]
mod tests {
    #[test]
    fn load_torrent_file() {
        assert!(true);
    }
}

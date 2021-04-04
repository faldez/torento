extern crate pretty_env_logger;
#[macro_use]
extern crate log;

mod metainfo;
mod peer;
mod torrent;
mod tracker;

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Read};

    use rand::{Rng, distributions::Alphanumeric};

    use crate::metainfo::Metainfo;
    use crate::peer;
    use crate::tracker::{self, announce};

    #[test]
    fn load_torrent_file() {

        
        
        assert!(true);
    }
}

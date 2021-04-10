use crate::{metainfo::Metainfo, tracker::Params};

pub struct Torrent {
    pub metainfo: Metainfo,
    pub params: Params,
}
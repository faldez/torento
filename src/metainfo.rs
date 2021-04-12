use std::collections::HashMap;

use serde::{Deserialize, Serialize, Deserializer, de::{Error, MapAccess, Visitor}};
use sha1::{Digest, Sha1};

#[derive(Debug, Deserialize, Serialize)]
pub struct Info {
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    #[serde(deserialize_with = "deserialize_pieces")]
    pub pieces: Vec<[u8; 20]>,
    pub length: Option<usize>,
    pub files: Option<Vec<File>>,
}

fn deserialize_pieces<'de, D>(deserializer: D) -> Result<Vec<[u8; 20]>, D::Error>
where
    D: Deserializer<'de>,
{
    struct PieceVisitor;

    impl<'de> Visitor<'de> for PieceVisitor {
        type Value = Vec<[u8; 20]>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("byte array")
        }

        fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
        where
            E: Error,
        {
            let mut pieces: Self::Value = vec![];
            let len = v.len();

            for i in (0..len).step_by(20) {
                let mut piece: [u8; 20] = [0; 20];
                piece.copy_from_slice(&v[i..i + 20]);

                pieces.push(piece);
            }

            Ok(pieces)
        }
    }

    deserializer.deserialize_bytes(PieceVisitor)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct File {
    pub length: usize,
    pub path: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Metainfo {
    pub announce: String,
    pub info: Info,
    #[serde(skip)]
    pub info_hash: [u8; 20],
}

impl Metainfo {
    pub fn from_bytes(buf: &[u8]) -> Self {
        let mut info: Self = serde_bencode::from_bytes(buf).unwrap();
        info.info_hash = info.create_info_hash(buf);

        info
    }
    
    fn create_info_hash(&mut self, buf: &[u8]) -> [u8; 20] {
        let info: HashMap<String, serde_bencode::value::Value> = serde_bencode::from_bytes(buf).unwrap();
        let info_bencoded = serde_bencode::to_bytes(&info[&"info".to_string()]).unwrap();
        
        let mut hasher = Sha1::new();
        hasher.update(info_bencoded);
        
        let result = hasher.finalize();
        
        let mut info_hash = [0; 20];
        info_hash.copy_from_slice(&result[..]);

        info_hash
    }
}

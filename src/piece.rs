use bitvec::prelude::*;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct Block {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Debug)]
pub struct Piece {
    pub index: usize,
    pub begin: usize,
    pub piece: Vec<u8>,
}

pub struct PieceCounter {
    pub total_pieces: u32,
    pub piece_index: BitVec<Msb0, u8>,
    pub downloaded: Vec<Vec<bool>>,
}

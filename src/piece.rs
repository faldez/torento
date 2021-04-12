use bitvec::prelude::*;

#[derive(Debug)]
pub struct Piece {
    pub index: usize,
    pub begin: usize,
    pub piece: Vec<u8>,
}

pub struct PieceCounter {
    pub total_pieces: u32,
    pub downloaded: BitVec<Msb0, u8>,
}
//! Piece verification for header probes (D117): blocks of one piece are hashed with SHA-1 as
//! their contiguous prefix grows (blocks that arrive out of order wait, keyed by offset: no ring
//! that a peer running ahead could overflow), and the piece becomes a [`VerifiedPiece`] only
//! when every byte is in and the digest is the one in the info dict.
//!
//! [`VerifiedPiece`] can only be made here, and upload mode can only serve bytes of a
//! [`VerifiedPiece`] ([`super::serve::UploadGate::hold`]): a byte fetched as an unverified block
//! can never reach a `piece` message.

use std::collections::BTreeMap;

use super::msg::BLOCK;

/// Why a piece could not be assembled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmError {
    /// A block that is not on the 16 KiB grid of the piece, or has the wrong length.
    BadBlock,
    /// Every byte arrived and the SHA-1 is not the one in the info dict. The peers (probe
    /// slots) that sent its blocks.
    Mismatch(Vec<usize>),
}

/// The blocks of one piece being fetched whole.
pub struct PieceAssembler {
    index: u32,
    len: u32,
    expected: [u8; 20],
    hasher: sha1_smol::Sha1,
    /// Bytes hashed so far (the contiguous prefix).
    next: u32,
    /// Blocks past the prefix, by offset.
    pending: BTreeMap<u32, Vec<u8>>,
    data: Vec<u8>,
    contributors: Vec<usize>,
}

impl PieceAssembler {
    pub fn new(index: u32, len: u32, expected: [u8; 20]) -> Self {
        Self {
            index,
            len,
            expected,
            hasher: sha1_smol::Sha1::new(),
            next: 0,
            pending: BTreeMap::new(),
            data: Vec::new(),
            contributors: Vec::new(),
        }
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    /// The expected length of the block at `begin`, None when `begin` is off the grid.
    pub fn block_len(&self, begin: u32) -> Option<u32> {
        (begin.is_multiple_of(BLOCK) && begin < self.len).then(|| (self.len - begin).min(BLOCK))
    }

    /// Add the block at `begin` sent by probe slot `peer`. A block already held is ignored.
    pub fn insert(&mut self, begin: u32, bytes: Vec<u8>, peer: usize) -> Result<(), AsmError> {
        let want = self.block_len(begin).ok_or(AsmError::BadBlock)?;
        if bytes.len() != want as usize {
            return Err(AsmError::BadBlock);
        }
        if begin < self.next || self.pending.contains_key(&begin) {
            return Ok(());
        }
        if !self.contributors.contains(&peer) {
            self.contributors.push(peer);
        }
        if begin > self.next {
            self.pending.insert(begin, bytes);
            return Ok(());
        }
        self.append(bytes);
        while let Some(b) = self.pending.remove(&self.next) {
            self.append(b);
        }
        Ok(())
    }

    fn append(&mut self, b: Vec<u8>) {
        self.hasher.update(&b);
        self.next += b.len() as u32;
        if self.data.is_empty() {
            self.data = b;
            self.data.reserve(self.len as usize - self.next as usize);
        } else {
            self.data.extend_from_slice(&b);
        }
    }

    pub fn is_complete(&self) -> bool {
        self.next == self.len
    }

    /// Bytes held (hashed prefix plus waiting blocks).
    pub fn held(&self) -> u64 {
        self.data.len() as u64 + self.pending.values().map(|b| b.len() as u64).sum::<u64>()
    }

    /// The verified piece, or the peers to strike. Call once [`Self::is_complete`].
    pub fn finish(mut self) -> Result<VerifiedPiece, AsmError> {
        if !self.is_complete() {
            return Err(AsmError::BadBlock);
        }
        if self.hasher.digest().bytes() != self.expected {
            self.data.fill(0);
            return Err(AsmError::Mismatch(std::mem::take(&mut self.contributors)));
        }
        Ok(VerifiedPiece {
            index: self.index,
            bytes: std::mem::take(&mut self.data),
        })
    }
}

impl Drop for PieceAssembler {
    fn drop(&mut self) {
        self.data.fill(0);
        for b in self.pending.values_mut() {
            b.fill(0);
        }
    }
}

/// A whole piece whose SHA-1 matched the info dict. Only [`PieceAssembler::finish`] makes one.
/// Its buffer is zeroed when dropped (copies made elsewhere are not: the docs say "not dumped
/// and not stored on purpose", never "nothing kept", check1 #1).
pub struct VerifiedPiece {
    index: u32,
    bytes: Vec<u8>,
}

impl VerifiedPiece {
    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Move the bytes out (the upload gate keeps them; the piece is then empty).
    pub(crate) fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

impl Drop for VerifiedPiece {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

impl std::fmt::Debug for VerifiedPiece {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedPiece")
            .field("index", &self.index)
            .field("len", &self.bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(len: usize) -> (Vec<u8>, [u8; 20]) {
        let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
        let h = sha1_smol::Sha1::from(&data).digest().bytes();
        (data, h)
    }

    #[test]
    fn out_of_order_blocks_verify() {
        let len = 3 * BLOCK as usize + 100;
        let (data, h) = piece(len);
        let mut a = PieceAssembler::new(5, len as u32, h);
        let blocks: Vec<(u32, Vec<u8>)> = data
            .chunks(BLOCK as usize)
            .enumerate()
            .map(|(i, c)| (i as u32 * BLOCK, c.to_vec()))
            .collect();
        for i in [3, 1, 0, 1, 2] {
            let (b, d) = &blocks[i];
            a.insert(*b, d.clone(), 0).unwrap();
        }
        assert!(a.is_complete());
        let v = a.finish().unwrap();
        assert_eq!((v.index(), v.bytes()), (5, data.as_slice()));
    }

    #[test]
    fn mismatch_names_the_peers_and_bad_blocks_are_refused() {
        let len = 2 * BLOCK as usize;
        let (mut data, h) = piece(len);
        data[10] ^= 1;
        let mut a = PieceAssembler::new(0, len as u32, h);
        assert_eq!(
            a.insert(1, vec![0; 10], 0),
            Err(AsmError::BadBlock),
            "off grid"
        );
        assert_eq!(a.insert(0, vec![0; 10], 0), Err(AsmError::BadBlock));
        assert_eq!(
            a.insert(2 * BLOCK, vec![0; 10], 0),
            Err(AsmError::BadBlock),
            "past the end"
        );
        a.insert(0, data[..BLOCK as usize].to_vec(), 1).unwrap();
        a.insert(BLOCK, data[BLOCK as usize..].to_vec(), 2).unwrap();
        assert_eq!(a.finish().unwrap_err(), AsmError::Mismatch(vec![1, 2]));
    }
}

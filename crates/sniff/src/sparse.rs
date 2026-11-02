//! The input model: a file of known length of which only some byte ranges are known.

use serde::Serialize;

/// A byte range `[offset, offset + len)` of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Range {
    pub offset: u64,
    pub len: u64,
}

impl Range {
    pub const fn new(offset: u64, len: u64) -> Range {
        Range { offset, len }
    }

    /// End offset (exclusive), saturating.
    pub const fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }

    /// The part of the range inside a file of `file_len` bytes, or `None` when nothing is left.
    pub fn clamp(self, file_len: u64) -> Option<Range> {
        if self.offset >= file_len || self.len == 0 {
            return None;
        }
        Some(Range::new(
            self.offset,
            self.len.min(file_len - self.offset),
        ))
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#x}+{}", self.offset, self.len)
    }
}

/// A file of `len` bytes of which some ranges are known (fetched from peers, or read locally).
///
/// Ranges are kept sorted, non-overlapping and coalesced (adjacent ranges are merged), so a read
/// that spans two fetches made back to back still succeeds. Bytes inserted beyond `len` are
/// dropped. Ranges inserted with [`SparseFile::insert_unverified`] are remembered: a detection
/// that read any of their bytes carries the `unverified` flag.
#[derive(Clone, Debug, Default)]
pub struct SparseFile {
    len: u64,
    name: Option<String>,
    ranges: Vec<(u64, Vec<u8>)>,
    unverified: Vec<Range>,
}

impl SparseFile {
    /// An empty view of a file of `len` bytes.
    pub fn new(len: u64) -> SparseFile {
        SparseFile {
            len,
            ..SparseFile::default()
        }
    }

    /// A fully known file.
    pub fn from_bytes(bytes: Vec<u8>) -> SparseFile {
        let mut f = SparseFile::new(bytes.len() as u64);
        f.insert(0, bytes);
        f
    }

    /// Sets the file name (path components are kept; only the last one is used).
    pub fn with_name(mut self, name: impl Into<String>) -> SparseFile {
        self.name = Some(name.into());
        self
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Known ranges, sorted by offset.
    pub fn ranges(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.ranges.iter().map(|(o, b)| (*o, b.as_slice()))
    }

    /// Ranges inserted as unverified (merged view not applied).
    pub fn unverified_ranges(&self) -> &[Range] {
        &self.unverified
    }

    /// Total number of known bytes.
    pub fn known_bytes(&self) -> u64 {
        self.ranges.iter().map(|(_, b)| b.len() as u64).sum()
    }

    /// Adds bytes known to be correct (hash-verified, or read locally).
    pub fn insert(&mut self, offset: u64, bytes: Vec<u8>) {
        self.insert_inner(offset, bytes);
    }

    /// Adds bytes that could not be verified (single 16 KiB blocks from a v1 torrent peer).
    pub fn insert_unverified(&mut self, offset: u64, bytes: Vec<u8>) {
        if let Some(r) = self.insert_inner(offset, bytes) {
            self.unverified.push(r);
        }
    }

    fn insert_inner(&mut self, offset: u64, mut bytes: Vec<u8>) -> Option<Range> {
        if offset >= self.len || bytes.is_empty() {
            return None;
        }
        let room = usize::try_from(self.len - offset).unwrap_or(usize::MAX);
        bytes.truncate(room);
        let end = offset.saturating_add(bytes.len() as u64);
        let added = Range::new(offset, bytes.len() as u64);
        // Ranges overlapping or touching [offset, end).
        let first = self
            .ranges
            .partition_point(|(o, b)| o.saturating_add(b.len() as u64) < offset);
        let last = self.ranges.partition_point(|(o, _)| *o <= end);
        if first >= last {
            self.ranges.insert(first, (offset, bytes));
            return Some(added);
        }
        let merged: Vec<(u64, Vec<u8>)> = self.ranges.drain(first..last).collect();
        let start = merged.first().map_or(offset, |(o, _)| (*o).min(offset));
        let stop = merged
            .iter()
            .map(|(o, b)| o.saturating_add(b.len() as u64))
            .max()
            .map_or(end, |e| e.max(end));
        let mut buf = vec![0u8; usize::try_from(stop - start).unwrap_or(0)];
        for (o, b) in merged
            .iter()
            .map(|(o, b)| (*o, b.as_slice()))
            .chain([(offset, bytes.as_slice())])
        {
            let s = usize::try_from(o - start).unwrap_or(usize::MAX);
            if let Some(dst) = s.checked_add(b.len()).and_then(|e| buf.get_mut(s..e)) {
                dst.copy_from_slice(b);
            }
        }
        self.ranges.insert(first, (start, buf));
        Some(added)
    }

    /// Bytes `[offset, offset + len)` when fully known.
    pub fn get(&self, offset: u64, len: usize) -> Option<&[u8]> {
        let i = self
            .ranges
            .partition_point(|(o, _)| *o <= offset)
            .checked_sub(1)?;
        let (o, b) = self.ranges.get(i)?;
        let s = usize::try_from(offset - o).ok()?;
        b.get(s..s.checked_add(len)?)
    }

    /// The longest known run of bytes starting at `offset` (empty when unknown).
    pub fn from_offset(&self, offset: u64) -> &[u8] {
        let Some(i) = self
            .ranges
            .partition_point(|(o, _)| *o <= offset)
            .checked_sub(1)
        else {
            return &[];
        };
        let Some((o, b)) = self.ranges.get(i) else {
            return &[];
        };
        usize::try_from(offset - o)
            .ok()
            .and_then(|s| b.get(s..))
            .unwrap_or(&[])
    }

    /// The known bytes that end at EOF (empty when the last byte is unknown).
    pub fn known_suffix(&self) -> &[u8] {
        match self.ranges.last() {
            Some((o, b)) if o.saturating_add(b.len() as u64) == self.len => b,
            _ => &[],
        }
    }

    /// True when `[offset, offset + len)`, clamped to the file end, is known.
    pub fn has(&self, offset: u64, len: u64) -> bool {
        let len = len.min(self.len.saturating_sub(offset));
        usize::try_from(len).is_ok_and(|l| self.get(offset, l).is_some())
    }

    /// The parts of `r` (clamped to the file) not known yet.
    pub fn missing(&self, r: Range) -> Vec<Range> {
        let Some(r) = r.clamp(self.len) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut pos = r.offset;
        let end = r.end();
        for (o, b) in &self.ranges {
            let e = o.saturating_add(b.len() as u64);
            if e <= pos {
                continue;
            }
            if *o >= end {
                break;
            }
            if *o > pos {
                out.push(Range::new(pos, o - pos));
            }
            pos = pos.max(e);
            if pos >= end {
                break;
            }
        }
        if pos < end {
            out.push(Range::new(pos, end - pos));
        }
        out
    }

    /// True when any byte of `[offset, offset + len)` came from an unverified insert.
    pub fn touches_unverified(&self, offset: u64, len: u64) -> bool {
        let end = offset.saturating_add(len);
        self.unverified
            .iter()
            .any(|u| u.offset < end && offset < u.end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_adjacent_and_overlapping() {
        let mut f = SparseFile::new(100);
        f.insert(10, vec![1; 10]);
        f.insert(20, vec![2; 5]);
        assert_eq!(f.ranges().count(), 1);
        assert_eq!(f.get(18, 4), Some(&[1, 1, 2, 2][..]));
        f.insert(0, vec![3; 12]);
        assert_eq!(f.ranges().count(), 1);
        assert_eq!(f.get(0, 1), Some(&[3][..]));
        assert_eq!(f.from_offset(24), &[2]);
        f.insert(50, vec![4; 200]);
        assert_eq!(f.known_bytes(), 25 + 50);
        assert_eq!(f.missing(Range::new(0, 100)), vec![Range::new(25, 25)]);
        assert!(f.has(60, 1000));
        assert!(!f.has(24, 2));
        f.insert(1000, vec![1]);
        assert_eq!(f.ranges().count(), 2);
    }

    #[test]
    fn unverified_is_tracked() {
        let mut f = SparseFile::new(100);
        f.insert(0, vec![0; 10]);
        f.insert_unverified(10, vec![0; 10]);
        assert!(f.touches_unverified(15, 1));
        assert!(!f.touches_unverified(0, 10));
    }
}

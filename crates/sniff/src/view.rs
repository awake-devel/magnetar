//! Per-call read access to a [`SparseFile`] for the checks: tracks unverified reads and the
//! nesting depth of compressed peeks.

use std::cell::Cell;

use crate::magic::Signatures;
use crate::sparse::SparseFile;
use crate::types::Caps;

pub(crate) struct View<'a> {
    pub file: &'a SparseFile,
    pub caps: &'a Caps,
    pub depth: u8,
    /// The detector's signature set (compressed peeks label their payload with it).
    pub signatures: &'a Signatures,
    unverified_read: Cell<bool>,
}

impl<'a> View<'a> {
    pub fn new(
        file: &'a SparseFile,
        caps: &'a Caps,
        depth: u8,
        signatures: &'a Signatures,
    ) -> View<'a> {
        View {
            file,
            caps,
            depth,
            signatures,
            unverified_read: Cell::new(false),
        }
    }

    pub fn size(&self) -> u64 {
        self.file.len()
    }

    pub fn name(&self) -> &'a str {
        self.file.name().unwrap_or("")
    }

    /// Whether unverified bytes were read since the last call (and resets the mark).
    pub fn take_unverified(&self) -> bool {
        self.unverified_read.replace(false)
    }

    fn note(&self, off: u64, len: usize) {
        if !self.file.unverified_ranges().is_empty()
            && self.file.touches_unverified(off, len as u64)
        {
            self.unverified_read.set(true);
        }
    }

    /// Bytes `[off, off + len)` when fully known.
    pub fn at(&self, off: u64, len: usize) -> Option<&'a [u8]> {
        let b = self.file.get(off, len)?;
        self.note(off, len);
        Some(b)
    }

    /// Longest known run starting at `off`.
    pub fn from(&self, off: u64) -> &'a [u8] {
        let b = self.file.from_offset(off);
        if !b.is_empty() {
            self.note(off, b.len());
        }
        b
    }

    pub fn head(&self) -> &'a [u8] {
        self.from(0)
    }

    /// `[off, off + len)` clamped to EOF is known.
    pub fn has(&self, off: u64, len: u64) -> bool {
        self.file.has(off, len)
    }

    /// Known bytes ending at EOF (possibly empty).
    pub fn known_tail(&self) -> &'a [u8] {
        let t = self.file.known_suffix();
        if !t.is_empty() {
            self.note(self.size() - t.len() as u64, t.len());
        }
        t
    }

    /// The last `want` bytes (clamped to the file size) when known.
    pub fn tail(&self, want: usize) -> Option<&'a [u8]> {
        let want = (want as u64).min(self.size());
        self.at(self.size() - want, usize::try_from(want).ok()?)
    }
}

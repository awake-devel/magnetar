//! The parsers' view of a [`SparseFile`]: reads that miss record the range to fetch.
//!
//! Every container walker reads through [`Src::get`]. When the bytes are not known yet the
//! call returns `None` and remembers the range, rounded out to 16 KiB torrent blocks (a peer
//! sends whole blocks anyway, and the neighbouring bytes are usually the next thing a walker
//! wants). The walker then stops the chain that needed it and carries on with independent
//! chains, so one pass can ask for several ranges at once.
//!
//! Reads made inside [`Src::detail`] are second rank: while any first-rank range is wanted,
//! [`Src::take`] returns only those, so a budget that pays for whole pieces (D120) goes to
//! what names the tracks (codec, language) before what refines them (frame rate, first frames).

use std::cell::{Cell, RefCell};

use crate::sparse::{Range, SparseFile};

/// BitTorrent block size: requests are rounded out to it.
pub(crate) const BLOCK: u64 = 16 * 1024;

pub(crate) struct Src<'a> {
    f: &'a SparseFile,
    need: RefCell<Vec<Range>>,
    /// Wanted by reads inside [`Src::detail`]; asked only when `need` is empty.
    later: RefCell<Vec<Range>>,
    in_detail: Cell<bool>,
    notes: RefCell<Vec<String>>,
    max_range: u64,
    /// Some read touched bytes inserted with [`SparseFile::insert_unverified`].
    unverified: Cell<bool>,
}

impl<'a> Src<'a> {
    pub fn new(f: &'a SparseFile, max_range: u64) -> Src<'a> {
        Src {
            f,
            need: RefCell::new(Vec::new()),
            later: RefCell::new(Vec::new()),
            in_detail: Cell::new(false),
            notes: RefCell::new(Vec::new()),
            max_range: max_range.max(BLOCK),
            unverified: Cell::new(false),
        }
    }

    /// Marks the recon unverified when `[off, off + len)` touches an unverified insert.
    fn note_read(&self, off: u64, len: usize) {
        if !self.unverified.get()
            && !self.f.unverified_ranges().is_empty()
            && self.f.touches_unverified(off, len as u64)
        {
            self.unverified.set(true);
        }
    }

    /// Whether any read so far used unverified bytes.
    pub fn read_unverified(&self) -> bool {
        self.unverified.get()
    }

    pub fn len(&self) -> u64 {
        self.f.len()
    }

    /// `len` bytes at `off`, clamped to EOF. `None` (and a recorded need) when not known, or
    /// when `off` is at or past EOF (no need then).
    pub fn get(&self, off: u64, len: u64) -> Option<&'a [u8]> {
        if off >= self.f.len() {
            return None;
        }
        let len = len.min(self.f.len() - off);
        let n = usize::try_from(len).ok()?;
        match self.f.get(off, n) {
            Some(b) => {
                self.note_read(off, b.len());
                Some(b)
            }
            None => {
                self.want(off, len);
                None
            }
        }
    }

    /// Like [`Src::get`] but never records a need.
    pub fn peek(&self, off: u64, len: u64) -> Option<&'a [u8]> {
        if off >= self.f.len() {
            return None;
        }
        let len = len.min(self.f.len() - off);
        let b = self.f.get(off, usize::try_from(len).ok()?)?;
        self.note_read(off, b.len());
        Some(b)
    }

    /// The known bytes from `off` on, at most `max` (empty when unknown). Never records a need.
    pub fn avail(&self, off: u64, max: usize) -> &'a [u8] {
        let b = self.f.from_offset(off);
        let b = b.get(..max.min(b.len())).unwrap_or(b);
        if !b.is_empty() {
            self.note_read(off, b.len());
        }
        b
    }

    /// Records that `[off, off + len)` is wanted (block-aligned, clamped, capped).
    pub fn want(&self, off: u64, len: u64) {
        let size = self.f.len();
        if off >= size {
            return;
        }
        let len = len.clamp(1, self.max_range);
        let start = off - off % BLOCK;
        let end = off.saturating_add(len);
        let end = end.saturating_add((BLOCK - end % BLOCK) % BLOCK).min(size);
        let list = if self.in_detail.get() {
            &self.later
        } else {
            &self.need
        };
        for m in self.f.missing(Range::new(start, end.saturating_sub(start))) {
            let mut need = list.borrow_mut();
            if need.len() < 64 && !need.contains(&m) {
                need.push(m);
            }
        }
    }

    pub fn note(&self, s: impl Into<String>) {
        let mut n = self.notes.borrow_mut();
        let s = s.into();
        if n.len() < 32 && !n.contains(&s) {
            n.push(s);
        }
    }

    /// Runs `f` with its misses recorded as second rank: details (sample tables, first
    /// samples, fragments) that refine tracks the first-rank reads already named.
    pub fn detail<T>(&self, f: impl FnOnce() -> T) -> T {
        let was = self.in_detail.replace(true);
        let out = f();
        self.in_detail.set(was);
        out
    }

    pub fn wants_more(&self) -> bool {
        !self.need.borrow().is_empty() || !self.later.borrow().is_empty()
    }

    /// The wanted ranges (first rank only while any is wanted, else the details) and notes.
    pub fn take(self) -> (Vec<Range>, Vec<String>) {
        let mut need = self.need.into_inner();
        if need.is_empty() {
            need = self.later.into_inner();
        }
        need.sort();
        need.dedup();
        (need, self.notes.into_inner())
    }
}

//! Structural parsers: what a signature table cannot express (size fields checked against the
//! file, chains of frames, container walkers that ask for more ranges, compressed peeks).
//!
//! Each check sees the known bytes through a [`View`] and returns at most one [`Hit`].

use crate::types::Hit;
use crate::view::View;

pub(crate) mod archive;
pub(crate) mod console;
pub(crate) mod disc;
pub(crate) mod doc;
pub(crate) mod exe;
pub(crate) mod image;
pub(crate) mod media;
pub(crate) mod text;

pub(crate) type Check = fn(&View) -> Option<Hit>;

/// Every Rust check, in evaluation order (name, function).
pub(crate) fn checks() -> impl Iterator<Item = &'static (&'static str, Check)> {
    media::CHECKS
        .iter()
        .chain(image::CHECKS)
        .chain(doc::CHECKS)
        .chain(console::CHECKS)
        .chain(archive::CHECKS)
        .chain(exe::CHECKS)
        .chain(disc::CHECKS)
}

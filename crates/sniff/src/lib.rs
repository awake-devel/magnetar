//! Content-based file type detection and video recon on sparse input: byte ranges of a file,
//! as fetched from torrent peers.
//!
//! The extension never decides: the name only breaks ties between equally confident candidates
//! and raises the `extension_mismatch` / `suspicious_executable` flags. Detection is pure (no
//! I/O, no network, no unsafe): it reads what a [`SparseFile`] holds and answers
//! [`Outcome::Detected`], [`Outcome::NeedMore`] (with the ranges to fetch) or
//! [`Outcome::Unknown`]. [`detect_with`] is a driver that runs the fetch rounds against a
//! caller-supplied fetch function within the [`Caps`] budgets.
//!
//! Layers, in evaluation order (see `README.md` for the module map):
//! 1. Rust structural checks ([`formats`]): size fields checked against the file, frame chains,
//!    container walkers (ZIP central directory, MP4 boxes, Matroska, ISO directories, PE
//!    overlay, PS4/PS5 CNT), compressed peeks (gzip, zstd).
//! 2. TOML signature rules ([`magic`], `data/signatures/*.toml`, plus an optional user file).
//! 3. Text rules when nothing binary is confident, then a fallback (empty / zeros / unknown).
//!
//! ```
//! use sniff::{Outcome, SparseFile};
//! let mut f = SparseFile::new(1 << 20).with_name("clip.mkv");
//! f.insert(0, b"GIF89a\x10\x00\x20\x00\x00\x00\x00\x2C".to_vec());
//! let Outcome::Detected(d) = sniff::detect(&f) else { panic!() };
//! assert_eq!(d.label, "image.gif");
//! assert!(d.flags.extension_mismatch);
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

mod engine;
mod formats;
pub mod label;
pub mod magic;
mod name;
mod sparse;
mod types;
mod util;
pub mod video;
mod view;

pub use engine::{Analysis, Detector, Report, Stop, detect, detect_with, panics_caught};
pub use formats::console::listing;
pub use formats::console::{Content, GameInfo, Kind as GameKind, Platform, db_id_valid};
pub use magic::{SigError, Signatures};
pub use sparse::{Range, SparseFile};
pub use types::{Candidate, Caps, Detection, Field, Flags, Outcome, Value};
pub use video::{VideoInfo, VideoOutcome};

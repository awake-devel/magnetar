//! D110: the prices the fetcher and the parser charge, checked against real allocations.
//!
//! A counting allocator (per thread, so parallel tests do not mix; each allocation counted as
//! the chunk glibc's malloc gives it) measures the peak of a decode and the bytes an
//! `InfoSummary` keeps, and both must stay under what is priced:
//! `VALUE_COST` (112 B per value) and `STRING_COST` / `FILE_COST` (64 B per string and per file
//! on top of the string bytes). Its own binary, so the allocator does not touch other tests.
#![cfg(feature = "dht-crawl")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use magnetar_core::liveness::bencode::{self, Value};
use magnetar_core::metainfo::fetch::VALUE_COST;
use magnetar_core::metainfo::info::{FILE_COST, STRING_COST};
use magnetar_core::metainfo::{InfoSummary, output_max, parse_info_bounded};

struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

/// The chunk glibc's malloc takes for `n` bytes: `n` + an 8 B header, rounded to 16, at least 32.
fn chunk(n: usize) -> isize {
    (n.saturating_add(8 + 15) & !15).max(32) as isize
}

fn add(n: isize) {
    // `try_with`: the thread-locals may already be gone while a thread exits.
    let _ = LIVE.try_with(|l| {
        let v = l.get() + n;
        l.set(v);
        let _ = PEAK.try_with(|p| p.set(p.get().max(v)));
    });
}

// SAFETY: forwards every call to `System` unchanged; the counters are plain thread-locals.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        add(chunk(l.size()));
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        add(-chunk(l.size()));
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        add(chunk(new) - chunk(l.size()));
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn live() -> isize {
    LIVE.with(Cell::get)
}

/// Run `f`, returning its result, its peak above the starting point and what it kept.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    let start = live();
    PEAK.with(|p| p.set(start));
    let out = f();
    let peak = PEAK.with(Cell::get) - start;
    let kept = live() - start;
    (out, peak as usize, kept.max(0) as usize)
}

/// A tree leaf, with its 32-byte `pieces root` when `root` (BEP 52 gives every non-empty file
/// one; the decode test leaves it out to pack more values per byte).
fn leaf(len: i64, root: bool) -> Value {
    let mut props = vec![(b"length".to_vec(), Value::Int(len))];
    if root {
        props.push((b"pieces root".to_vec(), Value::Bytes(vec![b'r'; 32])));
    }
    Value::Dict(vec![(Vec::new(), Value::Dict(props))])
}

fn v2_with_tree(tree: Value) -> Vec<u8> {
    bencode::encode(&Value::Dict(vec![
        (b"file tree".to_vec(), tree),
        (b"meta version".to_vec(), Value::Int(2)),
        (b"name".to_vec(), Value::Bytes(b"t".to_vec())),
        (b"piece length".to_vec(), Value::Int(16384)),
    ]))
}

/// `n` leaves with distinct 3-byte names under `dir`.
fn tiny_leaves(dir: Vec<u8>, n: usize, root: bool) -> Value {
    let leaves = (0..n)
        .map(|i| {
            (
                vec![(i >> 16) as u8, (i >> 8) as u8, i as u8],
                leaf(1, root),
            )
        })
        .collect();
    Value::Dict(vec![(dir, Value::Dict(leaves))])
}

/// What the parser priced for a summary, recomputed from what it holds: every string at its
/// length plus `STRING_COST` (the name twice: once in `all_names`, once displayed) and every file
/// at `FILE_COST` plus its path.
fn priced(s: &InfoSummary) -> usize {
    let strings = 1 + s.files.len() + s.all_names.len();
    let bytes = s.name.len()
        + s.files.iter().map(|f| f.path.len()).sum::<usize>()
        + s.all_names.iter().map(String::len).sum::<usize>();
    bytes + strings * STRING_COST + s.files.len() * FILE_COST
}

#[test]
fn string_and_file_prices_cover_what_a_summary_keeps() {
    // Many short leaves: the per-string and per-file prices dominate, the worst case for them.
    for (dir, n) in [
        (vec![b'd'], 20_000),
        (vec![b'k'; 40], 5_000),
        (vec![0xFF; 8], 3_000),
    ] {
        let raw = v2_with_tree(tiny_leaves(dir, n, true));
        let (s, _, kept) = measure(|| parse_info_bounded(&raw, output_max(16 << 20)).unwrap());
        // The decoded tree is gone by now, so `kept` is the summary.
        eprintln!("{n} leaves: kept {kept} B, priced {} B", priced(&s));
        assert!(
            kept <= priced(&s),
            "{n} leaves: kept {kept} B > priced {} B",
            priced(&s)
        );
    }
}

#[test]
fn value_price_covers_a_decode_of_tiny_values() {
    // The reviewer's shape: a tree of tiny leaves, 5 values per leaf.
    let raw = v2_with_tree(tiny_leaves(b"d".to_vec(), 50_000, false));
    let values = bencode::count_values(&raw, usize::MAX);
    let (v, peak, _) = measure(|| bencode::decode(&raw).unwrap());
    drop(v);
    // The fetcher prices `3 × size + VALUE_COST × values`; one `size` is the input itself,
    // already held before the decode.
    let price = 2 * raw.len() + values * VALUE_COST;
    eprintln!(
        "{values} values, {} B: peak {peak} B ({} B per value), priced {price} B",
        raw.len(),
        peak / values
    );
    assert!(
        peak <= price,
        "{values} values: peak {peak} B > priced {price} B"
    );
}

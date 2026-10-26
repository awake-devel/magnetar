//! Print the parsed release attributes (D44) of names read from stdin, one JSON object per line.
//!
//! Input lines: `category<TAB>title` or `category<TAB>title<TAB>files` where `files` is a JSON
//! array of `{"path", "size"}` (what `torrents.files` holds). Used to build and re-check the
//! hand-labelled sample of docs/FILTERS.md:
//!
//! ```sh
//! cargo run -q -p magnetar-core --example release_attrs < titles.tsv
//! ```

use std::io::BufRead;

use magnetar_core::model::{Category, TorrentFile};
use magnetar_core::release::parse_release;

fn main() {
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let mut parts = line.splitn(3, '\t');
        let (Some(cat), Some(title)) = (parts.next(), parts.next()) else {
            continue;
        };
        let files: Vec<TorrentFile> = parts
            .next()
            .and_then(|f| serde_json::from_str(f).ok())
            .unwrap_or_default();
        let category: Category = cat.parse().unwrap_or(Category::Other);
        let attrs = parse_release(title, &files, category);
        let out = serde_json::json!({ "category": cat, "title": title, "attrs": attrs });
        println!("{out}");
    }
}

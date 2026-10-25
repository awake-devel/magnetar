//! Index schema (D43): one document per infohash, built from its non-blocked events.
//!
//! Text fields share one analyzer (split on anything but letters/digits, lowercase, ASCII
//! folding: `Été` == `ete`); no stop words, no stemming. Numbers are stored and fast (filters and
//! sorts read the fast columns). `FACETS` lists the keyword fields the facet API can count;
//! adding one (STAGE 2: language, resolution, …) means a new entry here and a `SCHEMA_VERSION`
//! bump, which rebuilds the index on the next start.

use magnetar_core::model::{Category, MediaKind};
use tantivy::schema::{
    FAST, Field, IndexRecordOption, NumericOptions, STORED, STRING, Schema, TextFieldIndexing,
    TextOptions,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
};
use tantivy::{TantivyDocument, Term};

/// Bump on any change to fields, analyzer or document contents: the index is rebuilt.
pub const SCHEMA_VERSION: u32 = 1;
pub const TOKENIZER: &str = "magnetar";
/// Descriptions are indexed up to this many characters (lower weight, mostly boilerplate).
pub const DESCRIPTION_CHARS: i32 = 2000;
/// File names are indexed up to this many characters of `files_text` per torrent.
pub const FILES_CHARS: i32 = 16 * 1024;

/// Unknown numbers are stored as this (sizes, peers, media fields).
pub const NONE: i64 = -1;
/// `publisher_trust` of a publisher with a local trust override: passes any threshold.
pub const TRUSTED: i64 = 1000;

pub fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(64))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// A keyword field the facet API counts (and later filters) on.
pub struct FacetDef {
    pub name: &'static str,
    pub values: fn(&IndexDoc) -> Vec<String>,
}

pub const FACETS: &[FacetDef] = &[
    FacetDef {
        name: "category",
        values: |d| vec![d.category.as_str().to_string()],
    },
    FacetDef {
        name: "category_path",
        values: |d| d.category_path.iter().map(|c| c.to_lowercase()).collect(),
    },
    FacetDef {
        name: "media_kind",
        values: |d| {
            d.media
                .iter()
                .map(|m| media_kind_str(m.kind).to_string())
                .collect()
        },
    },
    FacetDef {
        name: "genre",
        values: |d| d.media.iter().flat_map(|m| m.genres.clone()).collect(),
    },
    FacetDef {
        name: "year",
        values: |d| {
            d.media
                .iter()
                .filter_map(|m| m.year)
                .map(|y| y.to_string())
                .collect()
        },
    },
];

pub fn media_kind_str(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
        MediaKind::Anime => "anime",
    }
}

pub fn category_code(c: Category) -> u64 {
    Category::ALL.iter().position(|x| *x == c).unwrap_or(0) as u64
}

pub fn category_from_code(code: u64) -> Category {
    Category::ALL
        .get(code as usize)
        .copied()
        .unwrap_or(Category::Other)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocMedia {
    pub id: i64,
    pub kind: MediaKind,
    pub title: String,
    pub year: Option<i32>,
    pub rating: Option<f32>,
    pub genres: Vec<String>,
}

/// Everything one document holds, assembled from Postgres (`load::load_docs`).
#[derive(Clone, Debug, PartialEq)]
pub struct IndexDoc {
    pub infohash: String,
    /// Canonical event (highest trust, then oldest, then lowest id; D6).
    pub event_id: String,
    pub pubkey: String,
    /// Distinct publishers of the non-blocked events.
    pub publishers: Vec<String>,
    /// Per publisher (same order): its best event trust, `TRUSTED` with a local trust override.
    pub publisher_trust: Vec<i64>,
    /// Canonical title first, then other events' distinct titles.
    pub titles: Vec<String>,
    pub files_text: String,
    pub description: String,
    pub category: Category,
    pub category_path: Vec<String>,
    pub created_at: i64,
    pub total_size: Option<u64>,
    pub file_count: u32,
    /// The canonical event's trust (the highest).
    pub trust: u8,
    pub pow_bits: u8,
    pub source_url: Option<String>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub best_peers: Option<u32>,
    pub expires_at: Option<i64>,
    pub media: Option<DocMedia>,
}

#[derive(Clone)]
pub struct Fields {
    pub schema: Schema,
    pub infohash: Field,
    pub event_id: Field,
    pub pubkey: Field,
    pub publishers: Field,
    pub title: Field,
    pub files: Field,
    pub description: Field,
    pub source_url: Field,
    pub media_title: Field,
    pub media_kind: Field,
    pub category: Field,
    pub created_at: Field,
    pub total_size: Field,
    pub file_count: Field,
    pub trust: Field,
    /// Best `publisher_trust` (fast): the document passes `min_trust` iff this reaches it.
    pub eligible_trust: Field,
    /// Stored, one per publisher: publisher_count under a threshold = values reaching it.
    pub publisher_trust: Field,
    pub pow_bits: Field,
    pub seeders: Field,
    pub leechers: Field,
    pub best_peers: Field,
    pub expires_at: Field,
    pub media_id: Field,
    pub media_year: Field,
    /// IMDb-style rating × 10 (NONE when unknown).
    pub media_rating: Field,
    /// One keyword field per `FACETS` entry, same order.
    pub facets: Vec<Field>,
}

pub fn facet_field_name(name: &str) -> String {
    format!("f_{name}")
}

impl Fields {
    pub fn new() -> Self {
        let mut b = Schema::builder();
        let text = |stored: bool| {
            let o = TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(TOKENIZER)
                    .set_index_option(IndexRecordOption::WithFreqs),
            );
            if stored { o.set_stored() } else { o }
        };
        let num: NumericOptions = NumericOptions::default().set_fast().set_stored();
        let infohash = b.add_text_field("infohash", STRING | STORED);
        let event_id = b.add_text_field("event_id", STORED);
        let pubkey = b.add_text_field("pubkey", STORED);
        let publishers = b.add_text_field("publishers", STRING);
        let title = b.add_text_field("title", text(true));
        let files = b.add_text_field("files", text(false));
        let description = b.add_text_field("description", text(false));
        let source_url = b.add_text_field("source_url", STORED);
        let media_title = b.add_text_field("media_title", STORED);
        let media_kind = b.add_text_field("media_kind", STORED);
        let category = b.add_u64_field("category", num.clone());
        let created_at = b.add_i64_field("created_at", num.clone());
        let total_size = b.add_i64_field("total_size", num.clone());
        let file_count = b.add_i64_field("file_count", num.clone());
        let trust = b.add_i64_field("trust", num.clone());
        let eligible_trust =
            b.add_i64_field("eligible_trust", NumericOptions::default().set_fast());
        let publisher_trust =
            b.add_i64_field("publisher_trust", NumericOptions::default().set_stored());
        let pow_bits = b.add_i64_field("pow_bits", num.clone());
        let seeders = b.add_i64_field("seeders", num.clone());
        let leechers = b.add_i64_field("leechers", num.clone());
        let best_peers = b.add_i64_field("best_peers", num.clone());
        let expires_at = b.add_i64_field("expires_at", num.clone());
        let media_id = b.add_i64_field("media_id", num.clone());
        let media_year = b.add_i64_field("media_year", num.clone());
        let media_rating = b.add_i64_field("media_rating", num);
        let facets = FACETS
            .iter()
            .map(|f| b.add_text_field(&facet_field_name(f.name), STRING | FAST))
            .collect();
        Fields {
            schema: b.build(),
            infohash,
            event_id,
            pubkey,
            publishers,
            title,
            files,
            description,
            source_url,
            media_title,
            media_kind,
            category,
            created_at,
            total_size,
            file_count,
            trust,
            eligible_trust,
            publisher_trust,
            pow_bits,
            seeders,
            leechers,
            best_peers,
            expires_at,
            media_id,
            media_year,
            media_rating,
            facets,
        }
    }

    pub fn infohash_term(&self, infohash: &str) -> Term {
        Term::from_field_text(self.infohash, infohash)
    }

    pub fn facet(&self, name: &str) -> Option<Field> {
        FACETS
            .iter()
            .position(|f| f.name == name)
            .map(|i| self.facets[i])
    }

    pub fn document(&self, d: &IndexDoc) -> TantivyDocument {
        let opt = |v: Option<u32>| v.map_or(NONE, i64::from);
        let mut doc = TantivyDocument::new();
        doc.add_text(self.infohash, &d.infohash);
        doc.add_text(self.event_id, &d.event_id);
        doc.add_text(self.pubkey, &d.pubkey);
        for p in &d.publishers {
            doc.add_text(self.publishers, p);
        }
        for t in &d.titles {
            doc.add_text(self.title, t);
        }
        if !d.files_text.is_empty() {
            doc.add_text(self.files, &d.files_text);
        }
        if !d.description.is_empty() {
            doc.add_text(self.description, &d.description);
        }
        if let Some(u) = &d.source_url {
            doc.add_text(self.source_url, u);
        }
        doc.add_u64(self.category, category_code(d.category));
        doc.add_i64(self.created_at, d.created_at);
        doc.add_i64(
            self.total_size,
            d.total_size
                .and_then(|s| i64::try_from(s).ok())
                .unwrap_or(NONE),
        );
        doc.add_i64(self.file_count, i64::from(d.file_count));
        doc.add_i64(self.trust, i64::from(d.trust));
        doc.add_i64(
            self.eligible_trust,
            d.publisher_trust
                .iter()
                .copied()
                .max()
                .unwrap_or(i64::from(d.trust)),
        );
        for t in &d.publisher_trust {
            doc.add_i64(self.publisher_trust, *t);
        }
        doc.add_i64(self.pow_bits, i64::from(d.pow_bits));
        doc.add_i64(self.seeders, opt(d.seeders));
        doc.add_i64(self.leechers, opt(d.leechers));
        doc.add_i64(self.best_peers, opt(d.best_peers));
        doc.add_i64(self.expires_at, d.expires_at.unwrap_or(0));
        match &d.media {
            Some(m) => {
                doc.add_text(self.media_title, &m.title);
                doc.add_text(self.media_kind, media_kind_str(m.kind));
                doc.add_i64(self.media_id, m.id);
                doc.add_i64(self.media_year, m.year.map_or(NONE, i64::from));
                doc.add_i64(
                    self.media_rating,
                    m.rating.map_or(NONE, |r| (r * 10.0).round() as i64),
                );
            }
            None => {
                doc.add_i64(self.media_id, NONE);
                doc.add_i64(self.media_year, NONE);
                doc.add_i64(self.media_rating, NONE);
            }
        }
        for (def, field) in FACETS.iter().zip(&self.facets) {
            for v in (def.values)(d) {
                doc.add_text(*field, v);
            }
        }
        doc
    }
}

impl Default for Fields {
    fn default() -> Self {
        Self::new()
    }
}

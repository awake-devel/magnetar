//! Track corrections (ARCHITECTURE.md D128–D130, docs/PROBES.md §12): what the user checked
//! in VLC for each audio and subtitle track (language, kinds, name, a track that does not
//! exist, a track the probe missed), stored in `probe_corrections` apart from the detections
//! (`probe_files`) and never written by a probe.
//!
//! - [`anchor`] places each stored correction on the current tracks, on every read: the same
//!   position while the track list is unchanged, else the one track the snapshot still
//!   describes, else nowhere (`unplaced`, never applied to another track). Nothing is
//!   rewritten on a re-probe.
//! - [`apply`] gives the effective files (overrides applied, removed tracks marked, added
//!   tracks appended): the detail shows them and `probes::summary_of` builds the stored tags
//!   from them, so both search paths and the facet counts follow (D129).
//! - [`correct`] / [`remove`] are the API writes (D130); [`stats`] the detector accuracy.

use std::collections::{BTreeMap, HashMap, HashSet};

use magnetar_core::api::{
    ProbeCorrectionRequest, ProbeCorrectionStats, ProbeKindAccuracy, ProbeKindMissed,
};
use magnetar_core::model::{
    PROBE_AUDIO_KINDS, PROBE_MAX_TRACK_NAME, PROBE_SUB_KINDS, ProbeAudioTrack, ProbeFile,
    ProbeSubTrack, ProbeTrackDetected, ProbeTrackFix, ProbeUnplacedFix, ProbeUnplacedWas,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Json;

use crate::probes::{MAX_AUDIO_TRACKS, MAX_SUB_TRACKS, normalize_lang, summary_of};

/// Most correction rows per torrent.
pub const MAX_ROWS: usize = 256;
/// Most items per request (32 audio + 64 subtitle tracks).
pub const MAX_ITEMS: usize = MAX_AUDIO_TRACKS + MAX_SUB_TRACKS;
/// Longest `device`, bytes.
pub const MAX_DEVICE: usize = 32;

/// The track type of a correction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Track {
    Audio,
    Sub,
}

impl Track {
    pub fn as_str(self) -> &'static str {
        match self {
            Track::Audio => "audio",
            Track::Sub => "sub",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "audio" => Some(Track::Audio),
            "sub" => Some(Track::Sub),
            _ => None,
        }
    }
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Track::Audio => &PROBE_AUDIO_KINDS,
            Track::Sub => &PROBE_SUB_KINDS,
        }
    }
    fn noun(self) -> &'static str {
        match self {
            Track::Audio => "audio",
            Track::Sub => "subtitle",
        }
    }
    fn cap(self) -> usize {
        match self {
            Track::Audio => MAX_AUDIO_TRACKS,
            Track::Sub => MAX_SUB_TRACKS,
        }
    }
    fn name_track(self) -> sniff::video::NameTrack {
        match self {
            Track::Audio => sniff::video::NameTrack::Audio,
            Track::Sub => sniff::video::NameTrack::Subtitle,
        }
    }
}

/// What the detector said of the track when the user corrected it (`probe_corrections.detected`,
/// D128). `n` is the number of tracks of that type in the file; an added track's snapshot has
/// only `v` and `n`. `src` is where each detected kind came from (D130): `name` when the
/// track-name classifier gives it, else `flags`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub v: u16,
    pub n: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub src: BTreeMap<String, String>,
}

/// One `probe_corrections` row.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredFix {
    pub id: i64,
    /// `ProbeFile::i` of the file.
    pub file_i: u32,
    pub track: Track,
    /// The detected position it was made on (or last edited at); None for an added track.
    pub pos: Option<u16>,
    pub removed: bool,
    pub set_lang: bool,
    pub lang: Option<String>,
    pub set_kinds: bool,
    pub kinds: Vec<String>,
    pub set_name: bool,
    pub name: Option<String>,
    pub detected: Snapshot,
    pub device: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl StoredFix {
    /// The overridden fields, in the order `lang`, `kinds`, `name`.
    pub fn fields(&self) -> Vec<String> {
        [
            (self.set_lang, "lang"),
            (self.set_kinds, "kinds"),
            (self.set_name, "name"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| f.to_string())
        .collect()
    }
}

/// Where a correction applies on the current probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// On the detected track at this position.
    At(u16),
    /// An added track, appended after the detected ones.
    Added,
    /// Nowhere: `changed` (the track list changed and no track matches the snapshot) or
    /// `file` (the file is no longer probed, or has no video recon).
    Unplaced(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub id: i64,
    pub place: Place,
    /// The detection changed since the user checked (anchored), or the probe lists another
    /// number of tracks of that type (added): the user should look again.
    pub review: bool,
}

/// A track as anchoring compares it.
#[derive(Clone, Debug, Default, PartialEq)]
struct View {
    codec: String,
    channels: Option<u8>,
    lang: Option<String>,
    kinds: Vec<String>,
    name: Option<String>,
}

/// The detected tracks of type `t` in `f`; None when the file has no video recon.
fn views(f: &ProbeFile, t: Track) -> Option<Vec<View>> {
    let v = f.video.as_ref()?;
    Some(match t {
        Track::Audio => v
            .audio
            .iter()
            .map(|a| View {
                codec: a.codec.clone(),
                channels: a.channels,
                lang: a.lang.clone(),
                kinds: a.kinds.clone(),
                name: a.name.clone(),
            })
            .collect(),
        Track::Sub => v
            .subs
            .iter()
            .map(|s| View {
                codec: s.codec.clone(),
                channels: None,
                lang: s.lang.clone(),
                kinds: s.kinds.clone(),
                name: s.name.clone(),
            })
            .collect(),
    })
}

fn channels_compatible(a: Option<u8>, b: Option<u8>) -> bool {
    a.is_none() || b.is_none() || a == b
}

/// Place every correction on the current tracks (D128). `version` is the stored probe's
/// `probe_version`. Corrections are taken newest first (`updated_at`, then `id`), whatever
/// the order of `fixes`; a track claimed by one is not claimed by another, so the user's
/// latest intent wins. The result is in the order of `fixes`.
pub fn anchor(files: &[ProbeFile], version: u16, fixes: &[StoredFix]) -> Vec<Anchor> {
    let mut order: Vec<usize> = (0..fixes.len()).collect();
    order.sort_by(|&a, &b| {
        (fixes[b].updated_at, fixes[b].id).cmp(&(fixes[a].updated_at, fixes[a].id))
    });
    let mut claimed: HashSet<(u32, Track, usize)> = HashSet::new();
    let mut out: Vec<Option<Anchor>> = vec![None; fixes.len()];
    for i in order {
        let f = &fixes[i];
        let s = &f.detected;
        let placed = |place, review| {
            Some(Anchor {
                id: f.id,
                place,
                review,
            })
        };
        let list = files
            .iter()
            .find(|x| x.i == f.file_i)
            .and_then(|x| views(x, f.track));
        let Some(t) = list else {
            out[i] = placed(Place::Unplaced("file"), false);
            continue;
        };
        let Some(pos) = f.pos else {
            out[i] = placed(Place::Added, t.len() != usize::from(s.n));
            continue;
        };
        let p = usize::from(pos);
        let free = |j: usize| !claimed.contains(&(f.file_i, f.track, j));
        let codec_eq = |v: &View| s.codec.as_deref() == Some(v.codec.as_str());
        let same = t.len() == usize::from(s.n)
            && p < t.len()
            && codec_eq(&t[p])
            && channels_compatible(t[p].channels, s.channels)
            && free(p);
        let at = if same {
            Some(p)
        } else {
            // Moved: the one track the snapshot still describes. A version-1 snapshot has no
            // kinds and no name to compare (D127 re-probes). The candidates are taken among
            // every track, claimed or not, and the pick must then be free: a correction that
            // two tracks match stays unplaced even once the user checks one of them, so it is
            // never applied to the other one, a track the user never touched.
            let strict = s.v >= 2 && version >= 2;
            let candidates: Vec<usize> = (0..t.len())
                .filter(|&j| {
                    let v = &t[j];
                    codec_eq(v)
                        && v.lang == s.lang
                        && channels_compatible(v.channels, s.channels)
                        && (!strict || (v.kinds == s.kinds && v.name == s.name))
                })
                .collect();
            match candidates.as_slice() {
                [j] => Some(*j),
                c if c.contains(&p) => Some(p),
                _ => None,
            }
            .filter(|&j| free(j))
        };
        out[i] = match at {
            Some(j) => {
                claimed.insert((f.file_i, f.track, j));
                let d = &t[j];
                let review = if s.v >= 2 {
                    d.lang != s.lang || d.kinds != s.kinds || d.name != s.name
                } else {
                    d.lang != s.lang
                };
                placed(Place::At(j as u16), review)
            }
            None => placed(Place::Unplaced("changed"), false),
        };
    }
    out.into_iter().flatten().collect()
}

/// A correction that did not anchor ([`Place::Unplaced`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Unplaced {
    pub fix: StoredFix,
    pub reason: &'static str,
}

impl Unplaced {
    /// The detail's entry, with the file's path.
    pub fn info(&self, path: String) -> ProbeUnplacedFix {
        let f = &self.fix;
        let s = &f.detected;
        ProbeUnplacedFix {
            id: f.id,
            file: f.file_i,
            path,
            track: f.track.as_str().into(),
            pos: f.pos,
            reason: self.reason.into(),
            was: f.pos.map(|_| ProbeUnplacedWas {
                codec: s.codec.clone().unwrap_or_default(),
                channels: s.channels,
                lang: s.lang.clone(),
                kinds: s.kinds.clone(),
                name: s.name.clone(),
            }),
            fields: f.fields(),
            removed: f.removed,
            lang: f.lang.clone().filter(|_| f.set_lang),
            kinds: if f.set_kinds {
                f.kinds.clone()
            } else {
                Vec::new()
            },
            name: f.name.clone().filter(|_| f.set_name),
            at: f.updated_at,
        }
    }
}

fn fix_info(f: &StoredFix, review: bool, detected: Option<ProbeTrackDetected>) -> ProbeTrackFix {
    ProbeTrackFix {
        id: f.id,
        fields: f.fields(),
        removed: f.removed,
        added: f.pos.is_none(),
        review,
        detected,
        at: f.updated_at,
        by: f.device.clone(),
    }
}

/// Overrides and positions on one list of detected tracks (`ProbeAudioTrack` or
/// `ProbeSubTrack`, same field names), then the added tracks.
macro_rules! apply_list {
    ($list:expr, $file:expr, $track:expr, $fixes:expr, $anchors:expr, $added:expr) => {{
        for (j, t) in $list.iter_mut().enumerate() {
            t.pos = Some(j as u16);
        }
        for (f, a) in $fixes.iter().zip($anchors.iter()) {
            if f.file_i != $file || f.track != $track {
                continue;
            }
            let Place::At(j) = a.place else { continue };
            let Some(t) = $list.get_mut(usize::from(j)) else {
                continue;
            };
            let detected = ProbeTrackDetected {
                lang: t.lang.clone(),
                kinds: t.kinds.clone(),
                name: t.name.clone(),
            };
            let shown = f.set_lang || f.set_kinds || f.set_name || f.removed || a.review;
            if f.set_lang {
                t.lang = f.lang.clone();
            }
            if f.set_kinds {
                t.kinds = f.kinds.clone();
            }
            if f.set_name {
                t.name = f.name.clone();
            }
            t.fix = Some(fix_info(f, a.review, shown.then_some(detected)));
        }
        for (f, review) in $added {
            $list.push(Default::default());
            let t = $list.last_mut().expect("just pushed");
            t.lang = f.lang.clone().filter(|_| f.set_lang);
            if f.set_kinds {
                t.kinds = f.kinds.clone();
            }
            t.name = f.name.clone().filter(|_| f.set_name);
            t.fix = Some(fix_info(f, review, None));
        }
    }};
}

/// The effective files (D129): detected tracks in order, each with its `pos` and, when a
/// correction anchors to it, its overrides and `fix` (a removed track stays, marked); then
/// the added tracks (no codec), oldest first. And the corrections that did not anchor.
pub fn apply(
    files: &[ProbeFile],
    version: u16,
    fixes: &[StoredFix],
) -> (Vec<ProbeFile>, Vec<Unplaced>) {
    let anchors = anchor(files, version, fixes);
    let mut unplaced = Vec::new();
    for (f, a) in fixes.iter().zip(&anchors) {
        if let Place::Unplaced(reason) = a.place {
            unplaced.push(Unplaced {
                fix: f.clone(),
                reason,
            });
        }
    }
    let added = |file: u32, track: Track| -> Vec<(&StoredFix, bool)> {
        let mut v: Vec<(&StoredFix, bool)> = fixes
            .iter()
            .zip(&anchors)
            .filter(|(f, a)| a.place == Place::Added && f.file_i == file && f.track == track)
            .map(|(f, a)| (f, a.review))
            .collect();
        v.sort_by_key(|(f, _)| (f.created_at, f.id));
        v
    };
    let mut out = files.to_vec();
    for file in out.iter_mut() {
        let i = file.i;
        let Some(v) = file.video.as_mut() else {
            continue;
        };
        let audio: &mut Vec<ProbeAudioTrack> = &mut v.audio;
        apply_list!(
            audio,
            i,
            Track::Audio,
            fixes,
            anchors,
            added(i, Track::Audio)
        );
        let subs: &mut Vec<ProbeSubTrack> = &mut v.subs;
        apply_list!(subs, i, Track::Sub, fixes, anchors, added(i, Track::Sub));
    }
    (out, unplaced)
}

const COLUMNS: &str = "id, file_i, track, pos, removed, set_lang, lang, set_kinds, kinds, \
                       set_name, name, detected, device, created_at, updated_at";

type Row = (
    i64,
    i32,
    String,
    Option<i16>,
    bool,
    bool,
    Option<String>,
    bool,
    Vec<String>,
    bool,
    Option<String>,
    Json<Snapshot>,
    String,
    i64,
    i64,
);

fn of_row(r: Row) -> Option<StoredFix> {
    Some(StoredFix {
        id: r.0,
        file_i: u32::try_from(r.1).ok()?,
        track: Track::parse(&r.2)?,
        pos: r.3.and_then(|p| u16::try_from(p).ok()),
        removed: r.4,
        set_lang: r.5,
        lang: r.6,
        set_kinds: r.7,
        kinds: r.8,
        set_name: r.9,
        name: r.10,
        detected: r.11.0,
        device: r.12,
        created_at: r.13,
        updated_at: r.14,
    })
}

/// The corrections of `infohash`, newest first (`updated_at DESC, id DESC`).
pub async fn load<'e>(
    ex: impl sqlx::PgExecutor<'e>,
    infohash: &str,
) -> sqlx::Result<Vec<StoredFix>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM probe_corrections WHERE infohash = $1
         ORDER BY updated_at DESC, id DESC"
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(infohash)
        .fetch_all(ex)
        .await?;
    Ok(rows.into_iter().filter_map(of_row).collect())
}

// ---------------------------------------------------------------- writes (D130)

/// Why a correction request failed.
#[derive(Debug)]
pub enum FixError {
    /// 400 `invalid`.
    Invalid(String),
    /// 404 `not_found`.
    NotFound(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for FixError {
    fn from(e: sqlx::Error) -> Self {
        FixError::Db(e)
    }
}

/// A row to write: the values of a `probe_corrections` row but its id and times.
#[derive(Clone, Debug, PartialEq)]
pub struct NewFix {
    pub file_i: u32,
    pub track: Track,
    pub pos: Option<u16>,
    pub removed: bool,
    pub set_lang: bool,
    pub lang: Option<String>,
    pub set_kinds: bool,
    pub kinds: Vec<String>,
    pub set_name: bool,
    pub name: Option<String>,
    pub detected: Snapshot,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    Insert(NewFix),
    /// The row of this id, edited in place (`created_at` kept).
    Update(i64, NewFix),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub device: String,
    pub writes: Vec<Write>,
}

/// The languages a correction may set (ISO 639-2/B, sorted): the app's language picker
/// (`ProbeFormat.kt` `ISO639_2` without the /T codes, D131). `probes::normalize_lang` keeps
/// any three letters, so `xyz` would otherwise become a `s:xyz` tag no picker shows.
pub const CORRECTION_LANGS: [&str; 89] = [
    "afr", "alb", "amh", "ara", "arm", "aze", "baq", "bel", "ben", "bos", "bul", "bur", "cat",
    "chi", "cmn", "cze", "dan", "dut", "eng", "epo", "est", "fil", "fin", "fre", "geo", "ger",
    "gle", "glg", "gre", "guj", "haw", "heb", "hin", "hrv", "hun", "ice", "ind", "ita", "jpn",
    "kan", "kaz", "khm", "kor", "kur", "lao", "lat", "lav", "lit", "mac", "mal", "mao", "mar",
    "may", "mon", "nep", "nno", "nob", "nor", "pan", "per", "pol", "por", "pus", "rum", "rus",
    "sin", "slo", "slv", "som", "spa", "srp", "swa", "swe", "tam", "tat", "tel", "tgl", "tha",
    "tib", "tur", "ukr", "urd", "uzb", "vie", "wel", "xho", "yid", "yue", "zul",
];

/// A language as stored: None for absent, empty or `und`; else ISO 639-2/B
/// (`probes::normalize_lang`: `fr`, `fra` → `fre`, `mri` → `mao`) from
/// [`CORRECTION_LANGS`], or `detected`, the track's detected language (setting it back is a
/// check).
fn lang_of(l: &Option<String>, detected: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = l.as_deref().map(str::trim) else {
        return Ok(None);
    };
    if raw.is_empty() || raw.eq_ignore_ascii_case("und") {
        return Ok(None);
    }
    let code = if raw.eq_ignore_ascii_case("mri") {
        Some("mao".to_string())
    } else {
        normalize_lang(raw)
    };
    code.filter(|c| CORRECTION_LANGS.contains(&c.as_str()) || Some(c.as_str()) == detected)
        .map(Some)
        .ok_or_else(|| format!("unknown language code \"{raw}\""))
}

/// Kinds of the track type, no duplicates; sorted.
fn kinds_of(kinds: &[String], track: Track) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::with_capacity(kinds.len());
    for k in kinds {
        if !track.kinds().contains(&k.as_str()) {
            let article = if track == Track::Audio { "an" } else { "a" };
            return Err(format!("\"{k}\" is not {article} {} kind", track.noun()));
        }
        if out.contains(k) {
            return Err(format!("duplicate kind \"{k}\""));
        }
        out.push(k.clone());
    }
    out.sort();
    Ok(out)
}

/// A track name: trimmed, no control characters, at most 64 bytes; empty is none.
fn name_of(n: &Option<String>) -> Result<Option<String>, String> {
    let Some(n) = n.as_deref().map(str::trim) else {
        return Ok(None);
    };
    if n.chars().any(char::is_control) {
        return Err("name has control characters".into());
    }
    if n.len() > PROBE_MAX_TRACK_NAME {
        return Err(format!("name is longer than {PROBE_MAX_TRACK_NAME} bytes"));
    }
    Ok((!n.is_empty()).then(|| n.to_string()))
}

/// Where each kind of a detected track came from (D130): `name` when the track-name
/// classifier gives it from the stored name, else `flags` (container or bitstream flags,
/// `enm`, captions in the video). A kind both gave counts as `name` (sniff's own split is not
/// sent).
fn kind_sources(
    track: Track,
    kinds: &[String],
    name: Option<&str>,
    lang: Option<&str>,
) -> BTreeMap<String, String> {
    let by_name: Vec<&'static str> = name
        .map(|n| {
            sniff::video::classify_name(n, track.name_track(), lang)
                .kinds
                .iter()
                .map(|k| k.as_str())
                .collect()
        })
        .unwrap_or_default();
    kinds
        .iter()
        .map(|k| {
            let src = if by_name.contains(&k.as_str()) {
                "name"
            } else {
                "flags"
            };
            (k.clone(), src.to_string())
        })
        .collect()
}

fn snapshot_of(version: u16, t: &[View], pos: Option<usize>, track: Track) -> Snapshot {
    let mut s = Snapshot {
        v: version,
        n: u16::try_from(t.len()).unwrap_or(u16::MAX),
        ..Snapshot::default()
    };
    if let Some(d) = pos.and_then(|p| t.get(p)) {
        s.codec = Some(d.codec.clone());
        s.channels = d.channels.filter(|_| track == Track::Audio);
        s.lang = d.lang.clone();
        s.kinds = d.kinds.clone();
        s.name = d.name.clone();
        s.src = kind_sources(track, &d.kinds, d.name.as_deref(), d.lang.as_deref());
    }
    s
}

/// Check a request against the stored detection (`files`, `version`) and corrections
/// (`fixes`) and say what to write (D130). All items or none.
pub fn plan(
    req: &ProbeCorrectionRequest,
    files: &[ProbeFile],
    version: u16,
    fixes: &[StoredFix],
) -> Result<Plan, FixError> {
    let invalid = |m: String| FixError::Invalid(m);
    let device = req
        .device
        .as_deref()
        .map(str::trim)
        .unwrap_or("api")
        .to_string();
    if device.is_empty() || device.len() > MAX_DEVICE || device.chars().any(char::is_control) {
        return Err(invalid(format!(
            "device must be 1 to {MAX_DEVICE} printable bytes"
        )));
    }
    if req.items.is_empty() || req.items.len() > MAX_ITEMS {
        return Err(invalid(format!("items: 1 to {MAX_ITEMS} per request")));
    }
    let anchors = anchor(files, version, fixes);
    let anchored: HashMap<(u32, Track, u16), i64> = fixes
        .iter()
        .zip(&anchors)
        .filter_map(|(f, a)| match a.place {
            Place::At(p) => Some(((f.file_i, f.track, p), f.id)),
            _ => None,
        })
        .collect();
    let mut added: HashMap<(u32, Track), usize> = HashMap::new();
    for (f, a) in fixes.iter().zip(&anchors) {
        if a.place == Place::Added {
            *added.entry((f.file_i, f.track)).or_default() += 1;
        }
    }
    let mut grown: HashSet<(u32, Track)> = HashSet::new();
    let mut targets: HashSet<(u32, Track, Option<u16>, Option<i64>)> = HashSet::new();
    let mut inserts = 0usize;
    let mut writes = Vec::with_capacity(req.items.len());
    for (n, it) in req.items.iter().enumerate() {
        let e = |m: String| FixError::Invalid(format!("item {n}: {m}"));
        let track = Track::parse(&it.track)
            .ok_or_else(|| e("track must be \"audio\" or \"sub\"".into()))?;
        let t = files
            .iter()
            .find(|f| f.i == it.file)
            .and_then(|f| views(f, track))
            .ok_or_else(|| e(format!("no probed video file {}", it.file)))?;
        let (mut set_lang, mut set_kinds, mut set_name) = (false, false, false);
        for x in &it.set {
            match x.as_str() {
                "lang" => set_lang = true,
                "kinds" => set_kinds = true,
                "name" => set_name = true,
                other => return Err(e(format!("unknown field \"{other}\" in set"))),
            }
        }
        if it.removed && (it.pos.is_none() || it.id.is_some() || !it.set.is_empty()) {
            return Err(e("removed takes no other field".into()));
        }
        let detected_lang = it
            .pos
            .and_then(|p| t.get(usize::from(p)))
            .and_then(|d| d.lang.as_deref());
        let mut lang = if set_lang {
            lang_of(&it.lang, detected_lang).map_err(e)?
        } else {
            None
        };
        let mut kinds = if set_kinds {
            kinds_of(&it.kinds, track).map_err(e)?
        } else {
            Vec::new()
        };
        let mut name = if set_name {
            name_of(&it.name).map_err(e)?
        } else {
            None
        };
        match (it.id, it.pos) {
            (Some(_), Some(_)) => {
                return Err(e(
                    "give pos for a detected track or id for an added one, not both".into(),
                ));
            }
            (None, Some(pos)) => {
                let p = usize::from(pos);
                let Some(d) = t.get(p) else {
                    return Err(e(format!(
                        "no {} track at pos {pos} (the file has {})",
                        track.noun(),
                        t.len()
                    )));
                };
                if !targets.insert((it.file, track, Some(pos), None)) {
                    return Err(e("duplicate track".into()));
                }
                // A value equal to the detection is no override: "Save" of an unchanged
                // value is a check.
                if set_lang && lang == d.lang {
                    (set_lang, lang) = (false, None);
                }
                // `kinds` is sorted; a worker's detected kinds may come in any order.
                let mut detected_kinds = d.kinds.clone();
                detected_kinds.sort();
                if set_kinds && kinds == detected_kinds {
                    (set_kinds, kinds) = (false, Vec::new());
                }
                if set_name && name == d.name {
                    (set_name, name) = (false, None);
                }
                let nf = NewFix {
                    file_i: it.file,
                    track,
                    pos: Some(pos),
                    removed: it.removed,
                    set_lang,
                    lang,
                    set_kinds,
                    kinds,
                    set_name,
                    name,
                    detected: snapshot_of(version, &t, Some(p), track),
                };
                match anchored.get(&(it.file, track, pos)) {
                    Some(id) => writes.push(Write::Update(*id, nf)),
                    None => {
                        inserts += 1;
                        writes.push(Write::Insert(nf));
                    }
                }
            }
            (None, None) => {
                set_lang &= lang.is_some();
                set_kinds &= !kinds.is_empty();
                set_name &= name.is_some();
                if !(set_lang || set_kinds || set_name) {
                    return Err(e("an added track needs a language, a kind or a name".into()));
                }
                *added.entry((it.file, track)).or_default() += 1;
                grown.insert((it.file, track));
                inserts += 1;
                writes.push(Write::Insert(NewFix {
                    file_i: it.file,
                    track,
                    pos: None,
                    removed: false,
                    set_lang,
                    lang,
                    set_kinds,
                    kinds,
                    set_name,
                    name,
                    detected: snapshot_of(version, &t, None, track),
                }));
            }
            (Some(id), None) => {
                let Some(row) = fixes.iter().find(|f| f.id == id) else {
                    return Err(FixError::NotFound(format!(
                        "item {n}: no correction {id} on this torrent"
                    )));
                };
                if row.pos.is_some() {
                    return Err(e("use pos to correct a detected track".into()));
                }
                if row.file_i != it.file || row.track != track {
                    return Err(e(format!(
                        "correction {id} is on file {} ({})",
                        row.file_i,
                        row.track.as_str()
                    )));
                }
                if !targets.insert((it.file, track, None, Some(id))) {
                    return Err(e("duplicate track".into()));
                }
                // Fields not in `set` keep the row's values.
                if !set_lang {
                    lang = row.lang.clone().filter(|_| row.set_lang);
                }
                if !set_kinds {
                    kinds = if row.set_kinds {
                        row.kinds.clone()
                    } else {
                        Vec::new()
                    };
                }
                if !set_name {
                    name = row.name.clone().filter(|_| row.set_name);
                }
                let (set_lang, set_kinds, set_name) =
                    (lang.is_some(), !kinds.is_empty(), name.is_some());
                if !(set_lang || set_kinds || set_name) {
                    return Err(e(
                        "an added track needs a language, a kind or a name (undo it instead)"
                            .into(),
                    ));
                }
                writes.push(Write::Update(
                    id,
                    NewFix {
                        file_i: row.file_i,
                        track,
                        pos: None,
                        removed: false,
                        set_lang,
                        lang,
                        set_kinds,
                        kinds,
                        set_name,
                        name,
                        detected: snapshot_of(version, &t, None, track),
                    },
                ));
            }
        }
    }
    for (file, track) in &grown {
        let detected = files
            .iter()
            .find(|f| f.i == *file)
            .and_then(|f| views(f, *track))
            .map_or(0, |t| t.len());
        if detected + added.get(&(*file, *track)).copied().unwrap_or(0) > track.cap() {
            return Err(invalid("too many tracks".into()));
        }
    }
    if fixes.len() + inserts > MAX_ROWS {
        return Err(invalid("too many corrections for this torrent".into()));
    }
    Ok(Plan { device, writes })
}

/// Lock the probe row (404 without one) and read its version and stored detection. Both write
/// paths (this and `probes::store`) lock `probe_attrs` before reading `probe_corrections`.
async fn lock_probe(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    infohash: &str,
) -> Result<(u16, Vec<ProbeFile>), FixError> {
    let row: Option<(i16, Json<Vec<ProbeFile>>)> = sqlx::query_as(
        "SELECT p.probe_version, f.files FROM probe_attrs p JOIN probe_files f USING (infohash)
         WHERE p.infohash = $1 FOR UPDATE OF p",
    )
    .bind(infohash)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some((v, files)) => Ok((u16::try_from(v).unwrap_or(0), files.0)),
        None => {
            let known: Option<i32> =
                sqlx::query_scalar("SELECT 1 FROM dht_torrents WHERE infohash = $1")
                    .bind(infohash)
                    .fetch_optional(&mut **tx)
                    .await?;
            Err(FixError::NotFound(if known.is_some() {
                "no probe for this torrent".into()
            } else {
                "unknown infohash".into()
            }))
        }
    }
}

/// Recompute the `probe_attrs` columns tracks change from the effective tracks (D129). Only a
/// changed row is written, so its D122 trigger re-indexes the torrent (D43) only when a tag
/// or a language column changed.
async fn recompute(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    infohash: &str,
    version: u16,
    files: &[ProbeFile],
) -> sqlx::Result<()> {
    let fixes = load(&mut **tx, infohash).await?;
    let s = summary_of(&apply(files, version, &fixes).0);
    sqlx::query(
        "UPDATE probe_attrs SET audio_codecs = $2, audio_langs = $3, sub_langs = $4, tags = $5
         WHERE infohash = $1
           AND (audio_codecs, audio_langs, sub_langs, tags)
               IS DISTINCT FROM ($2::text[], $3::text[], $4::text[], $5::text[])",
    )
    .bind(infohash)
    .bind(&s.audio_codecs)
    .bind(&s.audio_langs)
    .bind(&s.sub_langs)
    .bind(&s.tags)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// `POST /api/v1/torrents/{infohash}/probe/corrections` (D130): check, write every item,
/// recompute the summary and tags, in one transaction.
pub async fn correct(
    pool: &PgPool,
    infohash: &str,
    req: &ProbeCorrectionRequest,
    now: i64,
) -> Result<(), FixError> {
    let mut tx = pool.begin().await?;
    let (version, files) = lock_probe(&mut tx, infohash).await?;
    let fixes = load(&mut *tx, infohash).await?;
    let plan = plan(req, &files, version, &fixes)?;
    for w in &plan.writes {
        let (id, f) = match w {
            Write::Insert(f) => (None, f),
            Write::Update(id, f) => (Some(*id), f),
        };
        let q = match id {
            None => sqlx::query(
                "INSERT INTO probe_corrections (file_i, track, pos, removed, set_lang, lang,
                     set_kinds, kinds, set_name, name, detected, device, created_at, updated_at,
                     infohash)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13, $14)",
            ),
            Some(_) => sqlx::query(
                "UPDATE probe_corrections SET file_i = $1, track = $2, pos = $3, removed = $4,
                     set_lang = $5, lang = $6, set_kinds = $7, kinds = $8, set_name = $9,
                     name = $10, detected = $11, device = $12, updated_at = $13
                 WHERE infohash = $14 AND id = $15",
            ),
        };
        q.bind(i32::try_from(f.file_i).unwrap_or(i32::MAX))
            .bind(f.track.as_str())
            .bind(f.pos.map(|p| i16::try_from(p).unwrap_or(i16::MAX)))
            .bind(f.removed)
            .bind(f.set_lang)
            .bind(&f.lang)
            .bind(f.set_kinds)
            .bind(&f.kinds)
            .bind(f.set_name)
            .bind(&f.name)
            .bind(Json(&f.detected))
            .bind(&plan.device)
            .bind(now)
            .bind(infohash)
            .bind(id.unwrap_or(0))
            .execute(&mut *tx)
            .await?;
    }
    recompute(&mut tx, infohash, version, &files).await?;
    tx.commit().await?;
    Ok(())
}

/// `DELETE /api/v1/torrents/{infohash}/probe/corrections/{id}` (D130): undo = back to the
/// detection.
pub async fn remove(pool: &PgPool, infohash: &str, id: i64) -> Result<(), FixError> {
    let mut tx = pool.begin().await?;
    let (version, files) = lock_probe(&mut tx, infohash).await?;
    let gone = sqlx::query("DELETE FROM probe_corrections WHERE infohash = $1 AND id = $2")
        .bind(infohash)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if gone == 0 {
        return Err(FixError::NotFound(format!(
            "no correction {id} on this torrent"
        )));
    }
    recompute(&mut tx, infohash, version, &files).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------- stats (D130)

/// Detector accuracy over every correction (D130, docs/PROBES.md §12), from the snapshots.
pub fn accuracy(fixes: &[StoredFix], now: i64) -> ProbeCorrectionStats {
    let mut s = ProbeCorrectionStats {
        at: now,
        ..Default::default()
    };
    let mut kinds: BTreeMap<(&'static str, String, String), (u64, u64)> = BTreeMap::new();
    let mut missed: BTreeMap<(&'static str, String), u64> = BTreeMap::new();
    for f in fixes {
        let d = &f.detected;
        if f.pos.is_none() {
            s.added += 1;
            continue;
        }
        s.tracks += 1;
        if f.removed {
            s.removed += 1;
            continue;
        }
        if f.set_lang || f.set_kinds || f.set_name {
            s.corrected += 1;
        } else {
            s.checked += 1;
        }
        if d.lang.is_some() {
            s.languages.checked += 1;
            if f.set_lang {
                s.languages.wrong += 1;
            }
        } else if f.set_lang && f.lang.is_some() {
            s.languages.filled += 1;
        }
        if f.set_name {
            s.names_changed += 1;
        }
        if d.v < 2 {
            continue;
        }
        let t = f.track.as_str();
        for k in &d.kinds {
            let src = d.src.get(k).cloned().unwrap_or_else(|| "flags".into());
            let e = kinds.entry((t, k.clone(), src)).or_default();
            e.0 += 1;
            if f.set_kinds && !f.kinds.contains(k) {
                e.1 += 1;
            }
        }
        if f.set_kinds {
            for k in f.kinds.iter().filter(|k| !d.kinds.contains(k)) {
                *missed.entry((t, k.clone())).or_default() += 1;
            }
        }
    }
    s.kinds = kinds
        .into_iter()
        .map(
            |((track, kind, source), (checked, wrong))| ProbeKindAccuracy {
                track: track.into(),
                kind,
                source,
                checked,
                wrong,
            },
        )
        .collect();
    s.kinds.sort_by(|a, b| {
        (b.wrong, b.checked)
            .cmp(&(a.wrong, a.checked))
            .then_with(|| (&a.track, &a.kind, &a.source).cmp(&(&b.track, &b.kind, &b.source)))
    });
    s.missed = missed
        .into_iter()
        .map(|((track, kind), count)| ProbeKindMissed {
            track: track.into(),
            kind,
            count,
        })
        .collect();
    // Stable: equal counts stay in (track, kind) order.
    s.missed.sort_by_key(|m| std::cmp::Reverse(m.count));
    s
}

/// `StatsResponse.probe_corrections`: one read of the (single-user, small) table.
pub async fn stats(pool: &PgPool, now: i64) -> sqlx::Result<ProbeCorrectionStats> {
    let sql = format!("SELECT {COLUMNS} FROM probe_corrections");
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await?;
    let fixes: Vec<StoredFix> = rows.into_iter().filter_map(of_row).collect();
    Ok(accuracy(&fixes, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn video_file(i: u32, size: u64, video: serde_json::Value) -> ProbeFile {
        ProbeFile {
            i,
            size,
            label: "video.mkv".into(),
            confidence: 97,
            verified: true,
            video: Some(serde_json::from_value(video).unwrap()),
            ..Default::default()
        }
    }

    fn with_subs(list: serde_json::Value) -> Vec<ProbeFile> {
        vec![video_file(
            0,
            1000,
            json!({"container": "matroska", "subs": list}),
        )]
    }

    /// A correction made on `files[0]` at `pos` at time `at` (snapshot of that detection).
    fn fix_on(
        id: i64,
        files: &[ProbeFile],
        version: u16,
        track: Track,
        pos: Option<u16>,
        at: i64,
    ) -> StoredFix {
        let t = views(&files[0], track).unwrap();
        StoredFix {
            id,
            file_i: files[0].i,
            track,
            pos,
            removed: false,
            set_lang: false,
            lang: None,
            set_kinds: false,
            kinds: Vec::new(),
            set_name: false,
            name: None,
            detected: snapshot_of(version, &t, pos.map(usize::from), track),
            device: "android".into(),
            created_at: at,
            updated_at: at,
        }
    }

    fn lang(mut f: StoredFix, l: Option<&str>) -> StoredFix {
        f.set_lang = true;
        f.lang = l.map(str::to_string);
        f
    }

    fn places(files: &[ProbeFile], v: u16, fixes: &[StoredFix]) -> Vec<(Place, bool)> {
        anchor(files, v, fixes)
            .into_iter()
            .map(|a| (a.place, a.review))
            .collect()
    }

    fn subs_of(files: &[ProbeFile]) -> Vec<ProbeSubTrack> {
        files[0].video.as_ref().unwrap().subs.clone()
    }

    /// The same track list: a correction stays on its track even when the detector changed
    /// its mind about the language (review), and applies its override there only.
    #[test]
    fn same_place_anchor() {
        let before = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "fre", "kinds": ["forced"]},
            {"codec": "hdmv_pgs_subtitle", "lang": "eng", "kinds": ["sdh"], "name": "English SDH"}]));
        let fixes = vec![
            lang(fix_on(1, &before, 2, Track::Sub, Some(1), 10), Some("ger")),
            fix_on(2, &before, 2, Track::Sub, Some(2), 11),
        ];
        assert_eq!(
            places(&before, 2, &fixes),
            [(Place::At(1), false), (Place::At(2), false)]
        );
        // A re-probe reads track 2's language differently: same place, flagged for review.
        let after = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "ita", "kinds": ["forced"]},
            {"codec": "hdmv_pgs_subtitle", "lang": "eng", "kinds": ["sdh"], "name": "English SDH"}]));
        assert_eq!(
            places(&after, 2, &fixes),
            [(Place::At(1), true), (Place::At(2), false)]
        );
        let (eff, unplaced) = apply(&after, 2, &fixes);
        assert!(unplaced.is_empty());
        let s = subs_of(&eff);
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.iter().map(|t| t.pos).collect::<Vec<_>>(),
            [Some(0), Some(1), Some(2)]
        );
        assert_eq!(s[0].fix, None);
        assert_eq!(s[1].lang.as_deref(), Some("ger"));
        assert_eq!(s[1].kinds, ["forced"]);
        let f = s[1].fix.as_ref().unwrap();
        assert_eq!(
            (f.id, f.fields.clone(), f.review),
            (1, vec!["lang".into()], true)
        );
        assert_eq!(
            f.detected,
            Some(ProbeTrackDetected {
                lang: Some("ita".into()),
                kinds: vec!["forced".into()],
                name: None
            })
        );
        // A check without review shows no detection; the track is unchanged.
        let f = s[2].fix.as_ref().unwrap();
        assert!(f.fields.is_empty() && f.detected.is_none() && !f.review && !f.added);
        assert_eq!(s[2].lang.as_deref(), Some("eng"));
        assert_eq!((f.at, f.by.as_str()), (11, "android"));
    }

    /// D123's TS expansion: teletext pages become tracks and shift the list; a correction
    /// follows the one track its snapshot still describes.
    #[test]
    fn moved_track_found_by_snapshot() {
        let ts = |subs: serde_json::Value| {
            vec![video_file(
                0,
                1000,
                json!({"container": "mpegts", "subs": subs}),
            )]
        };
        let before = ts(json!([
            {"codec": "dvb_teletext", "lang": "eng"},
            {"codec": "dvb_subtitle", "lang": "fre"}]));
        let mut kinds = fix_on(1, &before, 2, Track::Sub, Some(0), 10);
        kinds.set_kinds = true;
        kinds.kinds = vec!["sdh".into()];
        let fixes = vec![kinds, fix_on(2, &before, 2, Track::Sub, Some(1), 11)];
        let after = ts(json!([
            {"codec": "dvb_teletext", "lang": "fre"},
            {"codec": "dvb_teletext", "lang": "eng"},
            {"codec": "dvb_teletext", "lang": "ger"},
            {"codec": "dvb_subtitle", "lang": "fre"}]));
        assert_eq!(
            places(&after, 2, &fixes),
            [(Place::At(1), false), (Place::At(3), false)]
        );
        let s = subs_of(&apply(&after, 2, &fixes).0);
        assert_eq!(s[1].kinds, ["sdh"]);
        assert_eq!(s[1].pos, Some(1));
        assert!(s[0].fix.is_none() && s[2].fix.is_none());
        assert_eq!(s[3].fix.as_ref().map(|f| f.id), Some(2));
    }

    /// D127: a correction made on a version-1 probe (no kinds, no names) finds its track in
    /// the version-2 re-probe by codec and language; a version-2 snapshot also compares kinds
    /// and name.
    #[test]
    fn v1_snapshot_matches_v2_probe() {
        let v1 = with_subs(json!([
            {"codec": "subrip", "lang": "fre"},
            {"codec": "subrip", "lang": "eng"}]));
        let fixes = vec![lang(
            fix_on(1, &v1, 1, Track::Sub, Some(1), 10),
            Some("spa"),
        )];
        assert_eq!(fixes[0].detected.v, 1);
        let v2 = with_subs(json!([
            {"codec": "subrip", "lang": "fre", "kinds": ["forced"], "name": "Forced"},
            {"codec": "subrip", "lang": "fre"},
            {"codec": "subrip", "lang": "eng", "kinds": ["sdh"], "name": "English SDH"}]));
        // Kinds and name are new, not a change: no review for a version-1 snapshot.
        assert_eq!(places(&v2, 2, &fixes), [(Place::At(2), false)]);
        let s = subs_of(&apply(&v2, 2, &fixes).0);
        assert_eq!(s[2].lang.as_deref(), Some("spa"));
        assert_eq!(s[2].kinds, ["sdh"]);
        // The same snapshot taken as version 2 (no kinds, no name) describes no track.
        let mut strict = fixes[0].clone();
        strict.detected.v = 2;
        assert_eq!(
            places(&v2, 2, &[strict]),
            [(Place::Unplaced("changed"), false)]
        );
    }

    /// Two equal tracks and the correction's position among neither: unplaced, never applied
    /// to another track; a vanished codec too; a file no longer probed is `file`.
    #[test]
    fn ambiguous_or_changed_never_moves_to_another_track() {
        let before = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "ass", "lang": "fre"},
            {"codec": "hdmv_pgs_subtitle", "lang": "eng"}]));
        let mut gone_file = lang(fix_on(4, &before, 2, Track::Sub, Some(0), 13), Some("ita"));
        gone_file.file_i = 5;
        let fixes = vec![
            lang(fix_on(1, &before, 2, Track::Sub, Some(0), 10), Some("ger")),
            fix_on(2, &before, 2, Track::Sub, Some(1), 11),
            lang(fix_on(3, &before, 2, Track::Sub, Some(2), 12), Some("dut")),
            gone_file,
        ];
        let after = with_subs(json!([
            {"codec": "ass", "lang": "fre"},
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "eng"}]));
        assert_eq!(
            places(&after, 2, &fixes),
            [
                (Place::Unplaced("changed"), false),
                (Place::At(0), false),
                (Place::Unplaced("changed"), false),
                (Place::Unplaced("file"), false),
            ]
        );
        let (eff, unplaced) = apply(&after, 2, &fixes);
        let s = subs_of(&eff);
        assert!(s.iter().all(|t| t.lang.as_deref() != Some("ger")), "{s:?}");
        assert!(s[1].fix.is_none() && s[2].fix.is_none());
        let mut u: Vec<(i64, &str)> = unplaced.iter().map(|u| (u.fix.id, u.reason)).collect();
        u.sort();
        assert_eq!(u, [(1, "changed"), (3, "changed"), (4, "file")]);
        let info = unplaced
            .iter()
            .find(|u| u.fix.id == 1)
            .unwrap()
            .info("Film.mkv".into());
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({"id": 1, "file": 0, "path": "Film.mkv", "track": "sub", "pos": 0,
                   "reason": "changed", "was": {"codec": "subrip", "lang": "eng"},
                   "fields": ["lang"], "lang": "ger", "at": 10})
        );
        // Several candidates, the correction's own position among them: it stays there.
        let twins = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "eng"}]));
        let fixes = vec![fix_on(1, &twins, 2, Track::Sub, Some(1), 10)];
        let three = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "eng"}]));
        assert_eq!(places(&three, 2, &fixes), [(Place::At(1), false)]);
    }

    /// An ambiguous correction stays unplaced once the user checks one of the tracks it
    /// matches: claiming one candidate never makes the other one "unique" (review of D128).
    #[test]
    fn ambiguous_correction_stays_unplaced_when_a_twin_is_claimed() {
        let before = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "ass", "lang": "fre"}]));
        let x = lang(fix_on(1, &before, 2, Track::Sub, Some(0), 10), Some("ger"));
        let after = with_subs(json!([
            {"codec": "ass", "lang": "fre"},
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "eng"}]));
        assert_eq!(
            places(&after, 2, std::slice::from_ref(&x)),
            [(Place::Unplaced("changed"), false)]
        );
        // The user then checks subtitle track 2 in VLC.
        let y = fix_on(2, &after, 2, Track::Sub, Some(1), 20);
        let fixes = vec![x, y];
        assert_eq!(
            places(&after, 2, &fixes),
            [(Place::Unplaced("changed"), false), (Place::At(1), false)]
        );
        let (eff, unplaced) = apply(&after, 2, &fixes);
        let s = subs_of(&eff);
        assert_eq!(s[2].lang.as_deref(), Some("eng"));
        assert!(s[2].fix.is_none(), "{:?}", s[2]);
        assert_eq!(unplaced.len(), 1);
        assert_eq!(unplaced[0].fix.id, 1);
        assert!(!has(&summary_of(&eff).tags, "s:ger"));
    }

    /// Correction languages come from the picker's list (plus the track's own detected
    /// code); the server's ISO 639-1 and /T maps all land in it.
    #[test]
    fn correction_languages_are_known_codes() {
        let mut sorted = CORRECTION_LANGS.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, CORRECTION_LANGS, "sorted, no duplicates");
        for two in ["en", "fr", "de", "ja", "zh", "pt", "nb", "kn", "tl"] {
            let c = normalize_lang(two).unwrap();
            assert!(CORRECTION_LANGS.contains(&c.as_str()), "{two} → {c}");
        }
        for t in ["fra", "deu", "zho", "nld", "mya", "bod", "msa"] {
            let c = normalize_lang(t).unwrap();
            assert!(CORRECTION_LANGS.contains(&c.as_str()), "{t} → {c}");
        }
        let l = |s: &str, d: Option<&str>| lang_of(&Some(s.into()), d);
        assert_eq!(l("fra", None), Ok(Some("fre".into())));
        assert_eq!(l("mri", None), Ok(Some("mao".into())));
        assert_eq!(l("und", None), Ok(None));
        assert_eq!(l("xyz", None), Err("unknown language code \"xyz\"".into()));
        assert_eq!(l("tlh", Some("tlh")), Ok(Some("tlh".into())));
    }

    /// Two corrections that describe the same track: the newest (`updated_at`, then `id`)
    /// claims it whatever the order they are given in; the older one is unplaced.
    #[test]
    fn newest_correction_claims_first() {
        let files = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "ass", "lang": "fre"}]));
        let fixes = vec![
            lang(fix_on(1, &files, 2, Track::Sub, Some(0), 10), Some("fre")),
            lang(fix_on(2, &files, 2, Track::Sub, Some(0), 20), Some("ger")),
            lang(fix_on(3, &files, 2, Track::Sub, Some(1), 30), Some("ita")),
            lang(fix_on(4, &files, 2, Track::Sub, Some(1), 30), Some("spa")),
        ];
        assert_eq!(
            places(&files, 2, &fixes),
            [
                (Place::Unplaced("changed"), false),
                (Place::At(0), false),
                (Place::Unplaced("changed"), false),
                (Place::At(1), false),
            ]
        );
        let s = subs_of(&apply(&files, 2, &fixes).0);
        assert_eq!(s[0].lang.as_deref(), Some("ger"));
        assert_eq!(s[1].lang.as_deref(), Some("spa"));
    }

    /// Added tracks come after the detected ones, oldest first, without codec; once the probe
    /// lists another number of tracks of that type they ask for a review.
    #[test]
    fn added_track_review_when_count_changes() {
        let files = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "fre"}]));
        let mut spa = lang(fix_on(7, &files, 2, Track::Sub, None, 20), Some("spa"));
        spa.set_kinds = true;
        spa.kinds = vec!["forced".into()];
        spa.set_name = true;
        spa.name = Some("Español (forzados)".into());
        let cat = lang(fix_on(9, &files, 2, Track::Sub, None, 10), Some("cat"));
        assert_eq!(
            spa.detected,
            Snapshot {
                v: 2,
                n: 2,
                ..Default::default()
            }
        );
        let fixes = vec![spa, cat];
        assert_eq!(
            places(&files, 2, &fixes),
            [(Place::Added, false), (Place::Added, false)]
        );
        let s = subs_of(&apply(&files, 2, &fixes).0);
        assert_eq!(s.len(), 4);
        assert_eq!(
            s.iter().map(|t| t.lang.as_deref()).collect::<Vec<_>>(),
            [Some("eng"), Some("fre"), Some("cat"), Some("spa")]
        );
        assert_eq!((s[3].codec.as_str(), s[3].pos), ("", None));
        assert_eq!(s[3].kinds, ["forced"]);
        assert_eq!(s[3].name.as_deref(), Some("Español (forzados)"));
        let f = s[3].fix.as_ref().unwrap();
        assert!(f.added && !f.review && f.detected.is_none());
        assert_eq!(f.fields, ["lang", "kinds", "name"]);
        // The probe now lists three subtitle tracks: maybe the one the user added.
        let more = with_subs(json!([
            {"codec": "subrip", "lang": "eng"},
            {"codec": "subrip", "lang": "fre"},
            {"codec": "subrip", "lang": "spa", "kinds": ["forced"]}]));
        assert_eq!(
            places(&more, 2, &fixes),
            [(Place::Added, true), (Place::Added, true)]
        );
        let s = subs_of(&apply(&more, 2, &fixes).0);
        assert!(s[4].fix.as_ref().unwrap().review);
    }

    fn movie() -> Vec<ProbeFile> {
        vec![
            video_file(
                0,
                9000,
                json!({"container": "matroska",
                "video": [{"codec": "hevc", "width": 3840, "height": 2160}],
                "audio": [
                    {"codec": "truehd", "channels": 8, "lang": "eng", "atmos": true},
                    {"codec": "ac3", "channels": 2, "lang": "eng", "kinds": ["commentary"],
                     "name": "Commentary"}],
                "subs": [
                    {"codec": "subrip", "lang": "eng"},
                    {"codec": "hdmv_pgs_subtitle", "lang": "eng", "kinds": ["sdh"],
                     "name": "English SDH"}]}),
            ),
            // A sample: never the main file.
            video_file(
                1,
                10,
                json!({"container": "matroska", "audio": [{"codec": "aac", "lang": "jpn"}]}),
            ),
        ]
    }

    fn has(tags: &[String], t: &str) -> bool {
        tags.iter().any(|x| x == t)
    }

    /// The summary and tags come from the effective tracks (D129): overridden languages and
    /// kinds, added tracks (no `ac:` tag for their missing codec); corrections on a file
    /// other than the main one change nothing.
    #[test]
    fn effective_tracks_drive_tags() {
        let files = movie();
        let raw = summary_of(&files);
        assert!(has(&raw.tags, "a:eng") && has(&raw.tags, "sp:eng:sdh"));
        let audio0 = lang(fix_on(1, &files, 2, Track::Audio, Some(0), 10), Some("fre"));
        let sub1 = lang(fix_on(2, &files, 2, Track::Sub, Some(1), 11), Some("fre"));
        let mut added = lang(fix_on(3, &files, 2, Track::Audio, None, 12), Some("spa"));
        added.set_kinds = true;
        added.kinds = vec!["dub".into()];
        let mut sample = lang(
            fix_on(4, &files[1..], 2, Track::Audio, Some(0), 13),
            Some("kor"),
        );
        sample.file_i = 1;
        let fixes = vec![audio0, sub1, added, sample];
        let (eff, unplaced) = apply(&files, 2, &fixes);
        assert!(unplaced.is_empty());
        let s = summary_of(&eff);
        for t in [
            "a:fre",
            "a:spa",
            "ak:dub",
            "ap:spa:dub",
            "s:eng",
            "s:fre",
            "sk:sdh",
            "sp:fre:sdh",
            "ac:truehd",
            "ac:ac3",
            "x:atmos",
            "ap:eng:commentary",
        ] {
            assert!(has(&s.tags, t), "{t}: {:?}", s.tags);
        }
        for t in ["a:eng", "sp:eng:sdh", "ac:", "a:kor", "a:jpn"] {
            assert!(!has(&s.tags, t), "{t}: {:?}", s.tags);
        }
        assert_eq!(s.audio_codecs, ["truehd", "ac3"]);
        assert_eq!(s.audio_langs, ["fre", "eng", "spa"]);
        assert_eq!(s.sub_langs, ["eng", "fre"]);
        // No correction: the summary of the report as before.
        assert_eq!(summary_of(&apply(&files, 2, &[]).0), raw);
        // Everything else as detected.
        assert_eq!(
            (s.container, s.vcodec, s.height, s.files_verified, s.kind),
            (
                raw.container,
                raw.vcodec,
                raw.height,
                raw.files_verified,
                raw.kind
            )
        );
    }

    /// "This track doesn't exist": the track stays in the detail, marked, and gives no tag
    /// and no summary value.
    #[test]
    fn removed_track_leaves_every_tag() {
        let files = movie();
        let mut a = fix_on(1, &files, 2, Track::Audio, Some(0), 10);
        a.removed = true;
        let mut s = fix_on(2, &files, 2, Track::Sub, Some(1), 11);
        s.removed = true;
        let (eff, _) = apply(&files, 2, &[a, s]);
        let v = eff[0].video.as_ref().unwrap();
        assert_eq!((v.audio.len(), v.subs.len()), (2, 2));
        let fa = v.audio[0].fix.as_ref().unwrap();
        assert!(fa.removed && fa.fields.is_empty());
        assert_eq!(
            fa.detected.as_ref().and_then(|d| d.lang.as_deref()),
            Some("eng")
        );
        let sum = summary_of(&eff);
        for t in [
            "ac:truehd",
            "x:atmos",
            "a:eng",
            "sk:sdh",
            "sp:eng:sdh",
            "sf:image",
        ] {
            assert!(!has(&sum.tags, t), "{t}: {:?}", sum.tags);
        }
        assert!(has(&sum.tags, "ac:ac3") && has(&sum.tags, "s:eng") && has(&sum.tags, "sf:text"));
        assert_eq!(sum.audio_codecs, ["ac3"]);
        assert_eq!(sum.audio_langs, ["eng"], "the commentary track's language");
        assert_eq!(sum.sub_langs, ["eng"]);
    }

    /// D130: a detected kind the track-name classifier gives comes from the name, any other
    /// from the flags; the snapshot carries the sources.
    #[test]
    fn kind_sources_from_track_names() {
        let src = |track, kinds: &[&str], name: Option<&str>, lang: Option<&str>| {
            let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
            kind_sources(track, &kinds, name, lang)
                .into_iter()
                .collect::<Vec<_>>()
        };
        let p = |k: &str, s: &str| (k.to_string(), s.to_string());
        assert_eq!(
            src(
                Track::Sub,
                &["forced", "sdh"],
                Some("English SDH"),
                Some("eng")
            ),
            [p("forced", "flags"), p("sdh", "name")]
        );
        assert_eq!(
            src(Track::Sub, &["forced"], Some("Forced"), Some("eng")),
            [p("forced", "name")]
        );
        assert_eq!(
            src(Track::Sub, &["sdh"], None, Some("eng")),
            [p("sdh", "flags")]
        );
        assert_eq!(
            src(
                Track::Audio,
                &["commentary"],
                Some("Director's Commentary"),
                None
            ),
            [p("commentary", "name")]
        );
        assert_eq!(
            src(Track::Audio, &["description"], Some("English"), Some("eng")),
            [p("description", "flags")]
        );
        let files = movie();
        let f = fix_on(1, &files, 2, Track::Sub, Some(1), 10);
        assert_eq!(
            serde_json::to_value(&f.detected).unwrap(),
            json!({"v": 2, "n": 2, "codec": "hdmv_pgs_subtitle", "lang": "eng",
                   "kinds": ["sdh"], "name": "English SDH", "src": {"sdh": "name"}})
        );
        let a = fix_on(2, &files, 2, Track::Audio, Some(0), 10);
        assert_eq!(
            serde_json::to_value(&a.detected).unwrap(),
            json!({"v": 2, "n": 2, "codec": "truehd", "channels": 8, "lang": "eng"})
        );
        let added = fix_on(3, &files, 2, Track::Sub, None, 10);
        assert_eq!(
            serde_json::to_value(&added.detected).unwrap(),
            json!({"v": 2, "n": 2})
        );
    }

    fn row(
        track: Track,
        pos: Option<u16>,
        snap: serde_json::Value,
        edit: impl FnOnce(&mut StoredFix),
    ) -> StoredFix {
        let mut f = StoredFix {
            id: 0,
            file_i: 0,
            track,
            pos,
            removed: false,
            set_lang: false,
            lang: None,
            set_kinds: false,
            kinds: Vec::new(),
            set_name: false,
            name: None,
            detected: serde_json::from_value(snap).unwrap(),
            device: "api".into(),
            created_at: 1,
            updated_at: 1,
        };
        edit(&mut f);
        f
    }

    /// D130: every count from the snapshots, exactly.
    #[test]
    fn accuracy_stats_golden() {
        use Track::{Audio, Sub};
        let sdh = json!({"v": 2, "n": 9, "codec": "subrip", "lang": "eng", "kinds": ["sdh"],
                         "src": {"sdh": "name"}});
        let fixes = vec![
            // a: SDH from the name, right.
            row(Sub, Some(0), sdh.clone(), |_| {}),
            // b: SDH from the name, wrong.
            row(Sub, Some(1), sdh.clone(), |f| f.set_kinds = true),
            // c: language wrong, forced missed.
            row(
                Sub,
                Some(2),
                json!({"v": 2, "n": 9, "codec": "subrip", "lang": "fre"}),
                |f| {
                    f.set_lang = true;
                    f.lang = Some("ger".into());
                    f.set_kinds = true;
                    f.kinds = vec!["forced".into()];
                },
            ),
            // d: removed (no language, no kind counted).
            row(
                Sub,
                Some(3),
                json!({"v": 2, "n": 9, "codec": "subrip", "lang": "eng",
                                     "kinds": ["forced"]}),
                |f| f.removed = true,
            ),
            // e: language filled in, version 1.
            row(
                Audio,
                Some(0),
                json!({"v": 1, "n": 2, "codec": "ac3"}),
                |f| {
                    f.set_lang = true;
                    f.lang = Some("fre".into());
                },
            ),
            // f: name changed, commentary from the flags right.
            row(
                Audio,
                Some(1),
                json!({"v": 2, "n": 2, "codec": "ac3", "lang": "eng",
                                       "kinds": ["commentary"]}),
                |f| {
                    f.set_name = true;
                    f.name = Some("Commentaire".into());
                },
            ),
            // g: added.
            row(Sub, None, json!({"v": 2, "n": 9}), |f| {
                f.set_lang = true;
                f.lang = Some("spa".into());
            }),
            // h: kinds of a version-1 snapshot: neither checked nor missed.
            row(
                Sub,
                Some(4),
                json!({"v": 1, "n": 9, "codec": "subrip", "lang": "eng"}),
                |f| {
                    f.set_kinds = true;
                    f.kinds = vec!["forced".into()];
                },
            ),
            // i: two kinds missed, no language detected nor set.
            row(Sub, Some(5), json!({"v": 2, "n": 9, "codec": "ass"}), |f| {
                f.set_kinds = true;
                f.kinds = vec!["forced".into(), "signs".into()];
            }),
            // j: a language set to none on a track without one: not "filled".
            row(Sub, Some(6), json!({"v": 2, "n": 9, "codec": "ass"}), |f| {
                f.set_lang = true
            }),
        ];
        let s = accuracy(&fixes, 1_793_500_000);
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({
                "tracks": 9, "checked": 1, "corrected": 7, "removed": 1, "added": 1,
                "languages": {"checked": 5, "wrong": 1, "filled": 1},
                "names_changed": 1,
                "kinds": [
                    {"track": "sub", "kind": "sdh", "source": "name", "checked": 2, "wrong": 1},
                    {"track": "audio", "kind": "commentary", "source": "flags", "checked": 1,
                     "wrong": 0}],
                "missed": [
                    {"track": "sub", "kind": "forced", "count": 2},
                    {"track": "sub", "kind": "signs", "count": 1}],
                "at": 1_793_500_000
            })
        );
        assert_eq!(
            accuracy(&[], 5),
            ProbeCorrectionStats {
                at: 5,
                ..Default::default()
            }
        );
    }

    fn req(items: serde_json::Value) -> ProbeCorrectionRequest {
        serde_json::from_value(json!({"device": "android", "items": items})).unwrap()
    }

    fn err(r: Result<Plan, FixError>) -> String {
        match r {
            Err(FixError::Invalid(m)) => format!("400 {m}"),
            Err(FixError::NotFound(m)) => format!("404 {m}"),
            Err(FixError::Db(e)) => format!("db {e}"),
            Ok(p) => format!("ok {p:?}"),
        }
    }

    /// D130: what a request may say, item by item; the first error answers 400 (404 for an
    /// unknown id) and nothing is written.
    #[test]
    fn items_validated() {
        let mut files = movie();
        files.push(ProbeFile {
            i: 2,
            size: 5,
            label: "archive.zip".into(),
            ..Default::default()
        });
        let existing = vec![
            lang(fix_on(7, &files, 2, Track::Sub, None, 10), Some("spa")),
            fix_on(8, &files, 2, Track::Sub, Some(0), 11),
        ];
        let check = |items: serde_json::Value| err(plan(&req(items), &files, 2, &existing));
        let a = |extra: serde_json::Value| {
            let mut o = json!({"file": 0, "track": "sub", "pos": 1});
            o.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            json!([o])
        };
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (json!([]), "400 items: 1 to 96 per request"),
            (
                a(json!({"track": "video"})),
                "400 item 0: track must be \"audio\" or \"sub\"",
            ),
            (a(json!({"file": 2})), "400 item 0: no probed video file 2"),
            (a(json!({"file": 9})), "400 item 0: no probed video file 9"),
            (
                a(json!({"pos": 2})),
                "400 item 0: no subtitle track at pos 2 (the file has 2)",
            ),
            (
                a(json!({"set": ["lang"], "lang": "xx"})),
                "400 item 0: unknown language code \"xx\"",
            ),
            (
                a(json!({"set": ["lang"], "lang": "xyz"})),
                "400 item 0: unknown language code \"xyz\"",
            ),
            (
                a(json!({"set": ["kinds"], "kinds": ["music"]})),
                "400 item 0: \"music\" is not a subtitle kind",
            ),
            (
                a(json!({"track": "audio", "set": ["kinds"], "kinds": ["sdh"]})),
                "400 item 0: \"sdh\" is not an audio kind",
            ),
            (
                a(json!({"set": ["kinds"], "kinds": ["forced", "forced"]})),
                "400 item 0: duplicate kind \"forced\"",
            ),
            (
                a(json!({"set": ["name"], "name": "x".repeat(65)})),
                "400 item 0: name is longer than 64 bytes",
            ),
            (
                a(json!({"set": ["name"], "name": "a\u{7}b"})),
                "400 item 0: name has control characters",
            ),
            (
                a(json!({"set": ["codec"]})),
                "400 item 0: unknown field \"codec\" in set",
            ),
            (
                a(json!({"removed": true, "set": ["lang"], "lang": "fre"})),
                "400 item 0: removed takes no other field",
            ),
            (
                json!([{"file": 0, "track": "sub", "removed": true, "set": []}]),
                "400 item 0: removed takes no other field",
            ),
            (
                json!([{"id": 7, "file": 0, "track": "sub", "removed": true}]),
                "400 item 0: removed takes no other field",
            ),
            (
                json!([{"file": 0, "track": "sub", "pos": 0, "set": []},
                       {"file": 0, "track": "sub", "pos": 0, "removed": true}]),
                "400 item 1: duplicate track",
            ),
            (
                json!([{"id": 8, "file": 0, "track": "sub", "set": ["lang"], "lang": "fre"}]),
                "400 item 0: use pos to correct a detected track",
            ),
            (
                json!([{"id": 99, "file": 0, "track": "sub", "set": ["lang"], "lang": "fre"}]),
                "404 item 0: no correction 99 on this torrent",
            ),
            (
                json!([{"id": 7, "pos": 1, "file": 0, "track": "sub", "set": []}]),
                "400 item 0: give pos for a detected track or id for an added one, not both",
            ),
            (
                json!([{"file": 0, "track": "sub", "set": ["lang"]}]),
                "400 item 0: an added track needs a language, a kind or a name",
            ),
        ];
        for (items, want) in cases {
            assert_eq!(check(items.clone()), want, "{items}");
        }
        let many: Vec<serde_json::Value> = (0..97)
            .map(|_| json!({"file": 0, "track": "sub", "pos": 1, "set": []}))
            .collect();
        assert_eq!(check(json!(many)), "400 items: 1 to 96 per request");
        let bad_device = |d: serde_json::Value| {
            let r: ProbeCorrectionRequest = serde_json::from_value(
                json!({"device": d, "items": [{"file": 0, "track": "sub", "pos": 1}]}),
            )
            .unwrap();
            err(plan(&r, &files, 2, &existing))
        };
        assert_eq!(
            bad_device(json!(" ")),
            "400 device must be 1 to 32 printable bytes"
        );
        assert_eq!(
            bad_device(json!("d".repeat(33))),
            "400 device must be 1 to 32 printable bytes"
        );
        // Caps: 32 audio tracks in all, 256 rows per torrent.
        let full: Vec<serde_json::Value> = (0..31)
            .map(|_| json!({"codec": "aac", "lang": "eng"}))
            .collect();
        let big = vec![video_file(
            0,
            1,
            json!({"container": "matroska", "audio": full}),
        )];
        let add = json!([{"file": 0, "track": "audio", "set": ["lang"], "lang": "fre"}]);
        assert!(plan(&req(add.clone()), &big, 2, &[]).is_ok());
        let one_added = vec![lang(fix_on(1, &big, 2, Track::Audio, None, 1), Some("ita"))];
        assert_eq!(
            err(plan(&req(add.clone()), &big, 2, &one_added)),
            "400 too many tracks"
        );
        let rows: Vec<StoredFix> = (0..256)
            .map(|i| {
                let mut f = fix_on(i, &files, 2, Track::Sub, Some(1), i);
                f.pos = Some(1);
                f
            })
            .collect();
        assert_eq!(
            err(plan(
                &req(json!([{"file": 0, "track": "audio", "pos": 0, "set": []}])),
                &files,
                2,
                &rows
            )),
            "400 too many corrections for this torrent"
        );
        // An edit in place of the row anchored there is no new row.
        assert!(
            plan(
                &req(json!([{"file": 0, "track": "sub", "pos": 1, "set": []}])),
                &files,
                2,
                &rows
            )
            .is_ok()
        );
        // The spec's example request, all at once.
        let ok = plan(
            &req(json!([
                {"file": 0, "track": "sub", "pos": 1, "set": ["lang", "kinds"], "lang": "fr",
                 "kinds": ["forced"]},
                {"file": 0, "track": "audio", "pos": 0, "set": []},
                {"file": 0, "track": "audio", "pos": 1, "removed": true},
                {"file": 0, "track": "sub", "set": ["lang", "kinds", "name"], "lang": "spa",
                 "kinds": ["forced"], "name": " Español (forzados) "},
                {"id": 7, "file": 0, "track": "sub", "set": ["lang"], "lang": "cat"}
            ])),
            &files,
            2,
            &existing,
        )
        .unwrap();
        assert_eq!(ok.device, "android");
        assert_eq!(ok.writes.len(), 5);
        let Write::Insert(w) = &ok.writes[0] else {
            panic!("{:?}", ok.writes[0])
        };
        assert_eq!(
            (w.lang.as_deref(), w.kinds.clone()),
            (Some("fre"), vec!["forced".into()])
        );
        assert!(matches!(&ok.writes[2], Write::Insert(w) if w.removed && !w.set_lang));
        let Write::Insert(w) = &ok.writes[3] else {
            panic!()
        };
        assert_eq!(w.name.as_deref(), Some("Español (forzados)"));
        assert_eq!(
            w.detected,
            Snapshot {
                v: 2,
                n: 2,
                ..Default::default()
            }
        );
        let Write::Update(7, w) = &ok.writes[4] else {
            panic!("{:?}", ok.writes[4])
        };
        assert_eq!(w.lang.as_deref(), Some("cat"));
        // Defaults: no device is `api`.
        let r: ProbeCorrectionRequest =
            serde_json::from_value(json!({"items": [{"file": 0, "track": "sub", "pos": 1}]}))
                .unwrap();
        assert_eq!(plan(&r, &files, 2, &existing).unwrap().device, "api");
    }

    /// A value equal to the detection is no override: "Save" of an unchanged track is a
    /// check (codes normalised, kinds in any order, names trimmed); a correction anchored
    /// on the track is edited in place.
    #[test]
    fn equal_values_become_a_check() {
        let files = movie();
        let existing = vec![lang(
            fix_on(5, &files, 2, Track::Sub, Some(0), 10),
            Some("ger"),
        )];
        let p = plan(
            &req(json!([
                {"file": 0, "track": "sub", "pos": 1, "set": ["lang", "kinds", "name"],
                 "lang": "en", "kinds": ["sdh"], "name": "  English SDH "},
                {"file": 0, "track": "audio", "pos": 1, "set": ["kinds", "lang"],
                 "kinds": ["commentary"], "lang": "fre"},
                {"file": 0, "track": "sub", "pos": 0, "set": ["lang"], "lang": "ENG"},
                {"file": 0, "track": "audio", "pos": 0, "set": ["lang", "name"], "lang": "und"}
            ])),
            &files,
            2,
            &existing,
        )
        .unwrap();
        let w = |i: usize| match &p.writes[i] {
            Write::Insert(w) | Write::Update(_, w) => w.clone(),
        };
        let a = w(0);
        assert!(!a.set_lang && !a.set_kinds && !a.set_name, "{a:?}");
        assert_eq!(
            (a.lang.clone(), a.kinds.clone(), a.name.clone()),
            (None, vec![], None)
        );
        assert_eq!(a.detected.src.get("sdh").map(String::as_str), Some("name"));
        let b = w(1);
        assert!(b.set_lang && !b.set_kinds && !b.set_name);
        assert_eq!(b.lang.as_deref(), Some("fre"));
        // Back to the detected value on a corrected track: the row becomes a check, in place.
        assert!(matches!(&p.writes[2], Write::Update(5, w) if !w.set_lang && w.lang.is_none()));
        // `und` is "no language" (an override of `eng`); no name stays no name.
        let d = w(3);
        assert!(d.set_lang && d.lang.is_none() && !d.set_name);
        // A worker may list kinds in any order: the same set is still a check.
        let unsorted = with_subs(json!([{"codec": "subrip", "lang": "eng",
                                         "kinds": ["sdh", "forced"]}]));
        let p = plan(
            &req(
                json!([{"file": 0, "track": "sub", "pos": 0, "set": ["kinds"],
                         "kinds": ["forced", "sdh"]}]),
            ),
            &unsorted,
            2,
            &[],
        )
        .unwrap();
        assert!(matches!(&p.writes[0], Write::Insert(w) if !w.set_kinds && w.kinds.is_empty()));
    }
}

//! Track kinds (D123): what a subtitle or audio track is for (SDH, forced, signs & songs,
//! commentary, audio description, dub, ...), from the container and bitstream flags the parsers
//! set in [`Tk::kinds`] plus a multilingual classifier of the track name ([`classify_name`]).
//!
//! The merge rules are in [`resolve`] (docs/PROBES.md "Track kinds"): a container or bitstream
//! flag that is set wins, a flag that is absent or 0 is never negative evidence (except
//! Matroska FlagOriginal = 0, which says "dub"), name kinds are added, never subtracted, and a
//! name that says "full", "SDH" or "CC" cancels a forced flag the muxer set by mistake.
//!
//! The term lists beyond English and French are UNVERIFIED on real releases.

use serde::Serialize;

use super::Tk;

/// One kind of track. Declaration order is the alphabetical order of the snake_case names, so
/// `Ord` sorts like the strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Audio: an alternate mix (original mono, theatrical mix).
    Alternate,
    /// Subtitles: two languages in one track.
    Bilingual,
    /// Subtitles: closed captions of EIA-608 / 708 origin.
    Cc,
    /// Audio or subtitles of a commentary.
    Commentary,
    /// Audio description (audio) or text descriptions (subtitles) for visually impaired viewers.
    Description,
    /// Audio: dialogue-enhanced or clean audio.
    Dialogue,
    /// Audio: dubbed.
    Dub,
    /// Subtitles: easy reader.
    EasyReader,
    /// Subtitles: forced / foreign parts only.
    Forced,
    /// Subtitles: honorifics track (anime).
    Honorifics,
    /// Audio: karaoke (mapped to [`TrackKind::Signs`] on a subtitle track).
    Karaoke,
    /// Audio: music and effects, isolated score.
    Music,
    /// Audio: the original language.
    Original,
    /// Subtitles: SDH / hearing impaired.
    Sdh,
    /// Subtitles: signs & songs, songs, typesetting, lyrics, karaoke.
    Signs,
    /// Audio: voice-over (Lektor, MVO, DVO, AVO, SVO).
    Voiceover,
}

impl TrackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackKind::Alternate => "alternate",
            TrackKind::Bilingual => "bilingual",
            TrackKind::Cc => "cc",
            TrackKind::Commentary => "commentary",
            TrackKind::Description => "description",
            TrackKind::Dialogue => "dialogue",
            TrackKind::Dub => "dub",
            TrackKind::EasyReader => "easy_reader",
            TrackKind::Forced => "forced",
            TrackKind::Honorifics => "honorifics",
            TrackKind::Karaoke => "karaoke",
            TrackKind::Music => "music",
            TrackKind::Original => "original",
            TrackKind::Sdh => "sdh",
            TrackKind::Signs => "signs",
            TrackKind::Voiceover => "voiceover",
        }
    }
}

/// The track type a name is classified for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameTrack {
    Audio,
    Subtitle,
}

/// What a track name says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameClass {
    /// Sorted, no duplicates; already restricted to the track type's kinds.
    pub kinds: Vec<TrackKind>,
    /// Subtitles only: an explicit "full / complete / dialogue / sdh / cc" marker (cancels a
    /// forced flag).
    pub full: bool,
}

const SUB_KINDS: [TrackKind; 9] = [
    TrackKind::Bilingual,
    TrackKind::Cc,
    TrackKind::Commentary,
    TrackKind::Description,
    TrackKind::EasyReader,
    TrackKind::Forced,
    TrackKind::Honorifics,
    TrackKind::Sdh,
    TrackKind::Signs,
];

const AUDIO_KINDS: [TrackKind; 9] = [
    TrackKind::Alternate,
    TrackKind::Commentary,
    TrackKind::Description,
    TrackKind::Dialogue,
    TrackKind::Dub,
    TrackKind::Karaoke,
    TrackKind::Music,
    TrackKind::Original,
    TrackKind::Voiceover,
];

/// The kinds a track type may carry: Karaoke becomes Signs on a subtitle track, the rest of
/// the other type's kinds are dropped. Sorted, deduplicated.
fn restrict(kinds: impl IntoIterator<Item = TrackKind>, track: NameTrack) -> Vec<TrackKind> {
    let mut v: Vec<TrackKind> = kinds
        .into_iter()
        .filter_map(|k| match track {
            NameTrack::Subtitle if k == TrackKind::Karaoke => Some(TrackKind::Signs),
            NameTrack::Subtitle => SUB_KINDS.contains(&k).then_some(k),
            NameTrack::Audio => AUDIO_KINDS.contains(&k).then_some(k),
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The primary subtag of a language code, lowercase (`en-US` → `en`).
fn primary(lang: Option<&str>) -> String {
    lang.and_then(|l| l.trim().split(['-', '_']).next())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// A track name cut into tokens (docs/PROBES.md "Track kinds", normalising).
struct Name {
    /// Tokens in their original case.
    orig: Vec<String>,
    /// The same, Unicode lowercase.
    low: Vec<String>,
    /// The lowercase name without whitespace (CJK substring matches).
    cjk: String,
}

impl Name {
    fn new(s: &str) -> Name {
        let norm: String = s
            .chars()
            .take(256)
            .filter(|c| *c != '\'' && *c != '\u{2019}')
            .map(|c| match c {
                '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
                '\u{3000}' => ' ',
                '+' => '&',
                c => c,
            })
            .collect();
        let mut orig = Vec::new();
        let mut cur = String::new();
        for c in norm.chars() {
            if c.is_alphanumeric() {
                cur.push(c);
                continue;
            }
            if !cur.is_empty() {
                orig.push(std::mem::take(&mut cur));
            }
            if c == '&' {
                orig.push("&".into());
            }
        }
        if !cur.is_empty() {
            orig.push(cur);
        }
        let low = orig.iter().map(|t| t.to_lowercase()).collect();
        let cjk = norm
            .to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        Name { orig, low, cjk }
    }

    /// `T{..}`: a whole lowercase token equals one of `set`.
    fn t(&self, set: &[&str]) -> bool {
        self.hit(|_, t| set.contains(&t))
    }

    /// Whether the token before position `i` negates it (`Non-Forced`, `No Commentary`,
    /// `Sans commentaire`, `Ohne Kommentar`): such a hit gives no kind.
    fn negated(&self, i: usize) -> bool {
        i.checked_sub(1)
            .and_then(|j| self.low.get(j))
            .is_some_and(|t| NEGATIONS.contains(&t.as_str()))
    }

    /// Whether some lowercase token that is not negated satisfies `f(index, token)`.
    fn hit(&self, f: impl Fn(usize, &str) -> bool) -> bool {
        self.low
            .iter()
            .enumerate()
            .any(|(i, t)| f(i, t) && !self.negated(i))
    }

    /// `P{..}`: a lowercase token starts with one of `set`.
    fn p(&self, set: &[&str]) -> bool {
        self.hit(|_, t| set.iter().any(|x| t.starts_with(x)))
    }

    /// `S[..]`: one of the phrases (space-separated tokens) appears as contiguous tokens.
    fn s(&self, phrases: &[&str]) -> bool {
        phrases.iter().any(|ph| {
            let words: Vec<&str> = ph.split(' ').collect();
            self.hit(|i, _| {
                self.low
                    .get(i..i + words.len())
                    .is_some_and(|w| w.iter().zip(&words).all(|(a, b)| a == b))
            })
        })
    }

    /// `U{..}`: a whole token in its original case equals one of `set`.
    fn u(&self, set: &[&str]) -> bool {
        self.orig
            .iter()
            .enumerate()
            .any(|(i, t)| set.contains(&t.as_str()) && !self.negated(i))
    }

    /// `C{..}`: the whitespace-free lowercase name contains one of `set`.
    fn c(&self, set: &[&str]) -> bool {
        set.iter().any(|x| self.cjk.contains(x))
    }

    /// `T{word}` unless the next token is one of `unless`.
    fn t_unless_next(&self, word: &str, unless: &[&str]) -> bool {
        self.hit(|i, t| {
            t == word
                && !self
                    .low
                    .get(i + 1)
                    .is_some_and(|n| unless.contains(&n.as_str()))
        })
    }
}

// ---------------------------------------------------------------- term lists

/// Words that negate the kind term right after them (review of D123): `Non-Forced`,
/// `Not Forced`, `Non-SDH`, `No Commentary`, `Without Commentary`, `Sans commentaire`,
/// `Ohne Kommentar`, `Sin comentarios`, `Senza commento`, `Zonder commentaar`,
/// `Utan kommentar`, `Uden`, `Uten`, `Ilman`, `Bez komentarza`, `Без комментариев`.
/// CJK substring matches are not negated. Phrases that start with a negation themselves
/// (`non udenti`, `non english`) still match: only the token before a hit is checked.
const NEGATIONS: &[&str] = &[
    "non", "no", "not", "without", "sans", "ohne", "sin", "senza", "zonder", "utan", "uden",
    "uten", "ilman", "bez", "без",
];

const SDH_T: &[&str] = &[
    "sdh",
    "psdh",
    "hoh",
    "deaf",
    "sme",
    "sourds",
    "malentendant",
    "malentendants",
    "sordos",
    "sordi",
    "nonudenti",
    "surdos",
    "doven",
    "slechthorenden",
    "hörselskadade",
    "hørehæmmede",
    "hørselshemmede",
    "kuulovammaisille",
    "kuulovammaiset",
    "ohjelmatekstitys",
    "niesłyszących",
    "nieslyszacych",
    "neslyšící",
    "hallássérült",
    "siketek",
    "слабослышащих",
];
const SDH_S: &[&str] = &[
    "hearing impaired",
    "hard of hearing",
    "for the deaf",
    "non udenti",
    "deficientes auditivos",
    "işitme engelli",
    "для глухих",
];
const SDH_P: &[&str] = &["hörgeschädigt", "hoergeschaedigt", "gehörlos"];
const SDH_C: &[&str] = &[
    "聴覚障害",
    "聴覚障碍",
    "听障",
    "聽障",
    "听力障碍",
    "聽力障礙",
    "청각장애",
];
const CC_T: &[&str] = &["cc", "closedcaptions"];
const CC_S: &[&str] = &["closed caption", "closed captions"];
const FORCED_T: &[&str] = &[
    "forced",
    "foreign",
    "forcé",
    "forcés",
    "forcée",
    "forcées",
    "forcee",
    "forcees",
    "forzado",
    "forzados",
    "forzada",
    "forzadas",
    "forzati",
    "forzato",
    "forçado",
    "forçados",
    "forçada",
    "forçadas",
    "erzwungen",
    "geforceerd",
    "tvungen",
    "tvungne",
    "pakotettu",
    "pakotetut",
    "wymuszone",
];
const FORCED_S: &[&str] = &["non english", "parts only"];
const FORCED_P: &[&str] = &["форсирован", "принудительн"];
const FORCED_C: &[&str] = &["強制", "强制", "강제"];
const FULL_T: &[&str] = &[
    "complete",
    "complets",
    "complet",
    "completos",
    "completo",
    "completi",
    "vollständig",
    "komplett",
    "volledig",
    "dialogue",
    "dialog",
    "dialogues",
];
const SIGNS_S: &[&str] = &[
    "signs & songs",
    "signs and songs",
    "songs & signs",
    "songs and signs",
    "s & s",
    "op ed",
    "op & ed",
];
const SIGNS_T: &[&str] = &[
    "signs",
    "sign",
    "songs",
    "song",
    "typesetting",
    "lyrics",
    "karaoke",
    "panneaux",
    "pancartes",
    "chansons",
];
const SIGNS_C: &[&str] = &["歌詞", "歌词", "特效"];
const COMMENTARY_T: &[&str] = &[
    "commentary",
    "commentaries",
    "commentaire",
    "commentaires",
    "comentario",
    "comentarios",
    "commento",
    "commenti",
    "commentaar",
    "kommentar",
    "kommentare",
    "kommentarspur",
    "audiocommento",
];
const COMMENTARY_P: &[&str] = &["комментари"];
const COMMENTARY_C: &[&str] = &[
    "コメンタリー",
    "評論音軌",
    "评论音轨",
    "導演評論",
    "导演评论",
    "코멘터리",
    "해설음성",
];
/// Japanese "audio commentary"; with バリアフリー or ガイド it is an audio description.
const ONSEI_KAISETSU: &str = "音声解説";
const BARRIER_FREE: &[&str] = &["バリアフリー", "ガイド"];
const TEXT_DESCRIPTION_S: &[&str] = &["text description", "text descriptions", "descriptive text"];
const HONORIFICS_T: &[&str] = &["honorifics", "honorific"];
const BILINGUAL_T: &[&str] = &["bilingual", "dualsub", "dualsubs"];
const BILINGUAL_S: &[&str] = &["dual subs", "dual sub", "chs & eng", "cht & eng"];
const BILINGUAL_C: &[&str] = &[
    "双语", "雙語", "中英", "简英", "繁英", "簡英", "中日", "简日", "繁日",
];
const EASY_T: &[&str] = &["easyreader", "lättläst", "selkokieli"];
const EASY_S: &[&str] = &["easy reader", "easy to read", "leichte sprache"];

const AD_T: &[&str] = &[
    "audiodescription",
    "audiodescribed",
    "descriptive",
    "audiodescripción",
    "audiodescripcion",
    "audiodescrição",
    "audiodescricao",
    "audiodescrizione",
    "audiodescriptie",
    "audiodeskription",
    "hörfilm",
    "hörfassung",
    "audiodeskrypcja",
    "syntolkning",
    "synstolkning",
    "synstolking",
    "kuvailutulkkaus",
];
const AD_S: &[&str] = &[
    "audio description",
    "audio described",
    "described video",
    "video description",
    "visually impaired",
];
const AD_U: &[&str] = &["AD", "DVS"];
const AD_P: &[&str] = &["тифлокомментари"];
const AD_C: &[&str] = &[
    "音声ガイド",
    "バリアフリー",
    "口述影像",
    "口述音轨",
    "화면해설",
];
const DIALOGUE_T: &[&str] = &["dialogueboost"];
const DIALOGUE_S: &[&str] = &[
    "dialogue boost",
    "dialog boost",
    "dialogue enhanced",
    "dialog enhanced",
    "dialogue enhancement",
    "clear dialogue",
    "clear dialog",
    "voice clarity",
    "clean audio",
    "speech enhanced",
];
const MUSIC_T: &[&str] = &["m&e", "instrumental"];
const MUSIC_S: &[&str] = &[
    "isolated score",
    "isolated music score",
    "score only",
    "music only",
    "m & e",
    "music & effects",
    "music and effects",
];
const ALTERNATE_S: &[&str] = &[
    "original mono",
    "original stereo",
    "original mix",
    "theatrical mix",
    "original theatrical mix",
    "home video mix",
    "restored mono",
];
const ORIGINAL_NOT_BEFORE: &[&str] =
    &["mono", "stereo", "mix", "score", "soundtrack", "theatrical"];
const ORIGINAL_T: &[&str] = &["orig", "originale", "originalton", "oton"];
const ORIGINAL_P: &[&str] = &["оригинал"];
const ORIGINAL_C: &[&str] = &["原音", "原声", "原聲", "オリジナル", "原語", "원어"];
const DUB_T: &[&str] = &[
    "dub",
    "dubbed",
    "dubbing",
    "doublage",
    "doublé",
    "doublée",
    "doblaje",
    "doblado",
    "doblada",
    "dublado",
    "dublada",
    "dublagem",
    "doppiaggio",
    "doppiato",
    "doppiata",
    "synchronfassung",
    "synchronisiert",
    "dabing",
    "szinkron",
    "dublaj",
];
const DUB_P: &[&str] = &["дубляж", "дублированн", "дубльован"];
const DUB_C: &[&str] = &["吹替", "吹き替え", "配音", "国配", "國配", "더빙"];
const VOICEOVER_T: &[&str] = &["lektor", "voiceover"];
const VOICEOVER_U: &[&str] = &["MVO", "DVO", "AVO", "SVO"];
const VOICEOVER_P: &[&str] = &[
    "закадров",
    "одноголос",
    "двухголос",
    "многоголос",
    "авторск",
];
/// Languages where `VO` means a voice-over (Russian, Ukrainian, Polish releases), not
/// "version originale".
const VO_IS_VOICEOVER: &[&str] = &["rus", "ukr", "pol", "ru", "uk", "pl"];

fn commentary(n: &Name) -> bool {
    n.t(COMMENTARY_T)
        || n.p(COMMENTARY_P)
        || n.c(COMMENTARY_C)
        || (n.c(&[ONSEI_KAISETSU]) && !n.c(BARRIER_FREE))
}

/// Classifies a track name (docs/PROBES.md "Track kinds"). `lang`: the track's language as
/// sniff holds it (`eng`, `en-US`, `fre`, ...), used for the `HI` (Hindi) and `VO`
/// (voice-over in ru / uk / pl) disambiguations.
pub fn classify_name(name: &str, track: NameTrack, lang: Option<&str>) -> NameClass {
    let n = Name::new(name);
    let lang = primary(lang);
    let mut k = Vec::new();
    let mut full = false;
    match track {
        NameTrack::Subtitle => {
            let hindi = lang == "hin" || lang == "hi";
            if n.t(SDH_T) || n.s(SDH_S) || n.p(SDH_P) || n.c(SDH_C) || (!hindi && n.u(&["HI"])) {
                k.push(TrackKind::Sdh);
                full = true;
            }
            if n.t(CC_T) || n.s(CC_S) {
                k.push(TrackKind::Cc);
                full = true;
            }
            if n.t(FORCED_T) || n.s(FORCED_S) || n.p(FORCED_P) || n.c(FORCED_C) {
                k.push(TrackKind::Forced);
            }
            if n.t(FULL_T) || n.t_unless_next("full", &["hd", "screen", "frame"]) {
                full = true;
            }
            // "Sign Language" is not a signs track: the pair is consumed.
            let signs_t = n.hit(|i, t| {
                SIGNS_T.contains(&t)
                    && !(t == "sign" && n.low.get(i + 1).is_some_and(|x| x == "language"))
            });
            if n.s(SIGNS_S) || signs_t || n.c(SIGNS_C) {
                k.push(TrackKind::Signs);
            }
            if commentary(&n) {
                k.push(TrackKind::Commentary);
            }
            if n.s(TEXT_DESCRIPTION_S) {
                k.push(TrackKind::Description);
            }
            if n.t(HONORIFICS_T) {
                k.push(TrackKind::Honorifics);
            }
            if n.t(BILINGUAL_T) || n.s(BILINGUAL_S) || n.c(BILINGUAL_C) {
                k.push(TrackKind::Bilingual);
            }
            if n.t(EASY_T) || n.s(EASY_S) {
                k.push(TrackKind::EasyReader);
            }
        }
        NameTrack::Audio => {
            let vo_voiceover = VO_IS_VOICEOVER.contains(&lang.as_str());
            if commentary(&n) {
                k.push(TrackKind::Commentary);
            }
            if n.t(AD_T)
                || n.s(AD_S)
                || n.u(AD_U)
                || n.p(AD_P)
                || n.c(AD_C)
                || (n.c(&[ONSEI_KAISETSU]) && n.c(BARRIER_FREE))
            {
                k.push(TrackKind::Description);
            }
            if n.t(DIALOGUE_T) || n.s(DIALOGUE_S) {
                k.push(TrackKind::Dialogue);
            }
            if n.t(MUSIC_T) || n.s(MUSIC_S) {
                k.push(TrackKind::Music);
            }
            if n.t(&["karaoke"]) {
                k.push(TrackKind::Karaoke);
            }
            if n.s(ALTERNATE_S) {
                k.push(TrackKind::Alternate);
            }
            if n.t_unless_next("original", ORIGINAL_NOT_BEFORE)
                || n.t(ORIGINAL_T)
                || n.s(&["o ton"])
                || n.u(&["VOF"])
                || (!vo_voiceover && n.u(&["VO"]))
                || n.p(ORIGINAL_P)
                || n.c(ORIGINAL_C)
            {
                k.push(TrackKind::Original);
            }
            if n.t(DUB_T) || n.p(DUB_P) || n.c(DUB_C) {
                k.push(TrackKind::Dub);
            }
            if n.t(VOICEOVER_T)
                || n.s(&["voice over"])
                || n.u(VOICEOVER_U)
                || (vo_voiceover && n.u(&["VO"]))
                || n.p(VOICEOVER_P)
            {
                k.push(TrackKind::Voiceover);
            }
        }
    }
    NameClass {
        kinds: restrict(k, track),
        full: full && track == NameTrack::Subtitle,
    }
}

/// The kinds of a track (docs/PROBES.md "Track kinds", merge rules): `(kinds, name_kinds)`,
/// both sorted; `name_kinds` is the subset of `kinds` only the name gave.
pub(crate) fn resolve(tk: &Tk, track: NameTrack) -> (Vec<TrackKind>, Vec<TrackKind>) {
    let flags = restrict(tk.kinds.iter().copied(), track);
    let name = tk
        .name
        .as_deref()
        .map(|s| classify_name(s, track, tk.language.as_deref()))
        .unwrap_or_default();
    let mut k: Vec<TrackKind> = flags.iter().chain(&name.kinds).copied().collect();
    // Kinds that come from a flag outside `Tk::kinds` (FlagForced, FlagOriginal, `enm`).
    let mut derived = Vec::new();
    let has = |k: &[TrackKind], x: TrackKind| k.contains(&x);
    match track {
        NameTrack::Subtitle => {
            if tk.forced {
                derived.push(TrackKind::Forced);
                k.push(TrackKind::Forced);
            }
            // A name that says "full", "SDH" or "CC" (and not "forced") wins over a forced flag.
            if (name.full || has(&name.kinds, TrackKind::Sdh) || has(&name.kinds, TrackKind::Cc))
                && !has(&name.kinds, TrackKind::Forced)
            {
                k.retain(|x| *x != TrackKind::Forced);
            }
            if primary(tk.language.as_deref()) == "enm" {
                derived.push(TrackKind::Honorifics);
                k.push(TrackKind::Honorifics);
            }
        }
        NameTrack::Audio => {
            match tk.original {
                Some(true) => {
                    derived.push(TrackKind::Original);
                    k.push(TrackKind::Original);
                }
                Some(false) => {
                    derived.push(TrackKind::Dub);
                    k.push(TrackKind::Dub);
                }
                None => {}
            }
            if [
                TrackKind::Commentary,
                TrackKind::Description,
                TrackKind::Music,
                TrackKind::Karaoke,
            ]
            .iter()
            .any(|x| k.contains(x))
            {
                k.retain(|x| *x != TrackKind::Original && *x != TrackKind::Dub);
            }
            if has(&k, TrackKind::Original) || has(&k, TrackKind::Voiceover) {
                k.retain(|x| *x != TrackKind::Dub);
            }
        }
    }
    let kinds = restrict(k, track);
    let name_kinds = name
        .kinds
        .iter()
        .copied()
        .filter(|x| kinds.contains(x) && !flags.contains(x) && !derived.contains(x))
        .collect();
    (kinds, name_kinds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::Kind;

    fn ids(k: &[TrackKind]) -> Vec<&'static str> {
        k.iter().map(|k| k.as_str()).collect()
    }

    #[test]
    fn name_vectors() {
        use NameTrack::{Audio as A, Subtitle as S};
        #[rustfmt::skip]
        let cases: &[(&str, NameTrack, &str, &[&str], bool)] = &[
            ("English (SDH)", S, "eng", &["sdh"], true),
            ("English [Forced]", S, "eng", &["forced"], false),
            ("Signs & Songs [GJM]", S, "eng", &["signs"], false),
            ("Full Subtitles [Group]", S, "eng", &[], true),
            ("English (CC)", S, "eng", &["cc"], true),
            ("Français (forcés)", S, "fre", &["forced"], false),
            ("English Commentary [USABD]", S, "eng", &["commentary"], false),
            ("Honorifics [Group]", S, "eng", &["honorifics"], false),
            ("简英双语", S, "chi", &["bilingual"], false),
            ("Full HD", S, "eng", &[], false),
            ("Forced Narrative", S, "eng", &["forced"], false),
            ("English (Hearing Impaired)", S, "eng", &["sdh"], true),
            ("Français SME", S, "fre", &["sdh"], true),
            ("Deutsch für Hörgeschädigte", S, "ger", &["sdh"], true),
            ("HI", S, "hin", &[], false),
            ("HI", S, "eng", &["sdh"], true),
            ("Easy Reader", S, "eng", &["easy_reader"], false),
            ("Ｆｏｒｃｅｄ", S, "eng", &["forced"], false),
            ("Sign Language", S, "eng", &[], false),
            ("Unforced", S, "eng", &[], false),
            ("Commentary by Director (SDH)", S, "eng", &["commentary", "sdh"], true),
            ("Karaoke", S, "jpn", &["signs"], false),
            ("English", S, "eng", &[], false),
            ("Commentary", A, "eng", &["commentary"], false),
            ("FLAC 2.0 - Descriptive", A, "eng", &["description"], false),
            ("English AD", A, "eng", &["description"], false),
            ("English ad", A, "eng", &[], false),
            ("Audiodescription", A, "fre", &["description"], false),
            ("English Dialogue Boost: Medium", A, "eng", &["dialogue"], false),
            ("Isolated Score", A, "eng", &["music"], false),
            ("Original Mono", A, "eng", &["alternate"], false),
            ("Japanese [Original]", A, "jpn", &["original"], false),
            ("VO", A, "fre", &["original"], false),
            ("VO", A, "rus", &["voiceover"], false),
            ("Lektor PL", A, "pol", &["voiceover"], false),
            ("MVO", A, "rus", &["voiceover"], false),
            ("Dubbed", A, "eng", &["dub"], false),
            ("Director's Commentary", A, "eng", &["commentary"], false),
            ("音声解説", A, "jpn", &["commentary"], false),
            ("音声解説 (バリアフリー)", A, "jpn", &["description"], false),
            ("Тифлокомментарий", A, "rus", &["description"], false),
            ("M&E", A, "eng", &["music"], false),
            ("English DTS-HD MA 5.1", A, "eng", &[], false),
            ("English (United Kingdom)", A, "eng", &[], false),
            // Negations (review of D123): the term right after a negation gives no kind.
            ("English (Non-Forced)", S, "eng", &[], false),
            ("Not Forced", S, "eng", &[], false),
            ("English (Full, Non-Forced)", S, "eng", &[], true),
            ("English Non-SDH", S, "eng", &[], false),
            ("No Commentary", S, "eng", &[], false),
            ("Without Commentary", A, "eng", &[], false),
            ("Sans commentaire", A, "fre", &[], false),
            ("Ohne Kommentar", S, "ger", &[], false),
            ("Non-Dubbed", A, "eng", &[], false),
            ("Italiano per non udenti", S, "ita", &["sdh"], true),
            ("Non English Parts", S, "eng", &["forced"], false),
        ];
        let mut bad = Vec::new();
        for (i, (name, track, lang, kinds, full)) in cases.iter().enumerate() {
            let c = classify_name(name, *track, Some(lang));
            if ids(&c.kinds) != *kinds || c.full != *full {
                bad.push(format!(
                    "#{} {name:?}: got {:?} full={}, want {kinds:?} full={full}",
                    i + 1,
                    ids(&c.kinds),
                    c.full
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    fn tk(kind: Kind, name: Option<&str>, lang: &str) -> Tk {
        Tk {
            kind,
            name: name.map(str::to_string),
            language: Some(lang.into()),
            ..Tk::default()
        }
    }

    #[test]
    fn resolve_rules() {
        let s = NameTrack::Subtitle;
        let a = NameTrack::Audio;
        // (a) A forced flag on a track whose name says "full" or "SDH".
        let mut t = tk(Kind::Sub, Some("English (Full)"), "eng");
        t.forced = true;
        assert_eq!(resolve(&t, s), (vec![], vec![]));
        t.name = Some("English (SDH)".into());
        assert_eq!(resolve(&t, s), (vec![TrackKind::Sdh], vec![TrackKind::Sdh]));
        // ... and a forced flag alone stays.
        t.name = Some("English".into());
        assert_eq!(resolve(&t, s), (vec![TrackKind::Forced], vec![]));
        // (b) No flag, the name says forced.
        let t = tk(Kind::Sub, Some("Forced"), "eng");
        assert_eq!(
            resolve(&t, s),
            (vec![TrackKind::Forced], vec![TrackKind::Forced])
        );
        // (c) FlagOriginal = 0: a dub, unless the name says commentary.
        let mut t = tk(Kind::Audio, None, "eng");
        t.original = Some(false);
        assert_eq!(resolve(&t, a), (vec![TrackKind::Dub], vec![]));
        t.name = Some("Commentary".into());
        assert_eq!(
            resolve(&t, a),
            (vec![TrackKind::Commentary], vec![TrackKind::Commentary])
        );
        // (d) FlagOriginal = 1 wins over a "Dubbed" name.
        let mut t = tk(Kind::Audio, Some("Dubbed"), "eng");
        t.original = Some(true);
        assert_eq!(resolve(&t, a), (vec![TrackKind::Original], vec![]));
        // (e) English with honorifics.
        let t = tk(Kind::Sub, None, "enm");
        assert_eq!(resolve(&t, s), (vec![TrackKind::Honorifics], vec![]));
        // Container kinds are kept and never repeated in name_kinds; karaoke is signs on a sub.
        let mut t = tk(Kind::Sub, Some("SDH"), "eng");
        t.kinds = vec![TrackKind::Sdh, TrackKind::Karaoke, TrackKind::Dub];
        assert_eq!(
            resolve(&t, s),
            (vec![TrackKind::Sdh, TrackKind::Signs], vec![])
        );
        // Voice-over removes a dub flag.
        let mut t = tk(Kind::Audio, Some("Lektor"), "pol");
        t.original = Some(false);
        assert_eq!(
            resolve(&t, a),
            (vec![TrackKind::Voiceover], vec![TrackKind::Voiceover])
        );
    }

    #[test]
    fn kinds_serialise_as_snake_case() {
        for k in AUDIO_KINDS.iter().chain(&SUB_KINDS) {
            assert_eq!(
                serde_json::to_string(k).unwrap(),
                format!("\"{}\"", k.as_str())
            );
        }
        let mut all: Vec<TrackKind> = AUDIO_KINDS.iter().chain(&SUB_KINDS).copied().collect();
        all.sort();
        all.dedup();
        let names: Vec<&str> = all.iter().map(|k| k.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "Ord follows the string order");
    }
}

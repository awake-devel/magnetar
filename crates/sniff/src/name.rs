//! The file name: tie-breaks and flags only, never the verdict.

use crate::label;
use crate::util::ext;

/// Extensions that strongly imply a type: a mismatch on these is worth flagging.
const KNOWN: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "mov", "wmv", "ts", "m2ts", "webm", "flv", "mpg", "mpeg", "vob",
    "iso", "img", "mp3", "flac", "ogg", "opus", "m4a", "wav", "aac", "ac3", "dts", "jpg", "jpeg",
    "png", "gif", "webp", "bmp", "pdf", "epub", "mobi", "azw3", "cbz", "cbr", "zip", "rar", "7z",
    "gz", "tar", "xz", "zst", "exe", "dll", "msi", "apk", "xapk", "pkg", "nsp", "xci", "srt",
    "ass", "ssa", "vtt", "sub", "idx", "sup", "nfo", "txt", "url", "torrent", "deb", "rpm", "dmg",
    "ttf", "otf", "woff2",
];

/// Names that promise media or documents (where executable content is suspicious).
const MEDIA: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "mov", "wmv", "ts", "m2ts", "webm", "mp3", "flac", "m4a", "srt",
    "jpg", "png", "pdf", "epub", "iso", "nfo", "txt",
];

/// Executable extensions used in double-extension tricks (`Movie.mkv.exe`).
const EXEC: &[&str] = &[
    "exe", "scr", "lnk", "bat", "cmd", "com", "pif", "vbs", "js", "msi", "hta",
];

/// Extensions of executables listed inside archives that make the archive suspicious.
pub(crate) const RISKY_INSIDE: &[&str] = &[
    "exe", "scr", "bat", "cmd", "lnk", "vbs", "js", "ps1", "msi", "com", "pif",
];

pub(crate) fn ext_is_known(e: &str) -> bool {
    KNOWN.contains(&e)
}

pub(crate) fn ext_matches(label_id: &str, e: &str) -> bool {
    label::extensions(label_id).contains(&e)
}

/// Bidi controls (RTLO and friends) and zero-width characters: never part of an honest name,
/// and RTLO turns `Movie\u{202e}vkm.exe` into "Movieexe.mkv" on screen.
fn is_hidden_control(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// The name as Windows resolves it: hidden controls removed, trailing dots and spaces
/// stripped (`Movie.mkv.exe.`), and spaces around each dot dropped (`Movie.mkv .exe`).
fn normalized(base: &str) -> String {
    let visible: String = base.chars().filter(|c| !is_hidden_control(*c)).collect();
    let trimmed = visible.trim_end_matches(['.', ' ']);
    trimmed
        .split('.')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(".")
        .to_lowercase()
}

/// `(extension_mismatch, suspicious_executable)` for a winning label.
pub(crate) fn flags(name: &str, label_id: &str, exe_inside: bool) -> (bool, bool) {
    let exe_like = label_id.starts_with("exe.")
        || matches!(
            label_id,
            "text.batch" | "text.vbscript" | "text.powershell" | "package.msi"
        )
        || exe_inside;
    let raw = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let hidden = raw.chars().any(is_hidden_control);
    let base = normalized(raw);
    let e = ext(&base);
    let parts: Vec<&str> = base.split('.').collect();
    let double = parts.len() >= 3
        && parts.iter().rev().nth(1).is_some_and(|p| ext_is_known(p))
        && parts.last().is_some_and(|p| EXEC.contains(p));
    let media_name = MEDIA.contains(&e.as_str());
    // A bidi or zero-width control in front of an executable is a disguise by itself.
    let disguised = hidden && (exe_like || EXEC.contains(&e.as_str()));
    let mismatch = !e.is_empty() && ext_is_known(&e) && !ext_matches(label_id, &e);
    (mismatch, double || disguised || (exe_like && media_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_extension_and_mismatch() {
        assert_eq!(flags("Movie.mkv.exe", "exe.pe", false), (false, true));
        assert_eq!(flags("Movie.mkv", "exe.pe", false), (true, true));
        assert_eq!(flags("Movie.mkv", "video.mkv", false), (false, false));
        assert_eq!(flags("setup.exe", "exe.pe.nsis", false), (false, false));
        assert_eq!(flags("pack.zip", "archive.zip", true), (false, false));
        assert_eq!(flags("Movie.iso", "archive.zip", true), (true, true));
    }

    /// Review finding: trailing dots / spaces (stripped by Windows), a space before the
    /// extension and RTLO all hid the double extension.
    #[test]
    fn name_tricks_are_seen_through() {
        for n in [
            "Movie.mkv.exe.",
            "Movie.mkv.exe ",
            "Movie.mkv .exe",
            "Movie.mkv. exe",
            "Movie\u{202e}vkm.exe",
            "Movie\u{202e}4pm.scr",
            "Movie.mkv\u{200b}.exe",
        ] {
            assert!(flags(n, "exe.pe", false).1, "{n:?}");
        }
        assert!(!flags("setup.exe.", "exe.pe", false).1);
        assert!(!flags("Movie.mkv ", "video.mkv", false).1);
        assert!(!flags("Movie.mkv.", "video.mkv", false).0);
    }
}

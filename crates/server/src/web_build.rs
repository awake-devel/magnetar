//! Rules for embedding the web UI (D132), shared by `build.rs` (which includes this file with
//! `#[path]`) and the library's unit tests. Standard library only: the build script has no
//! dependencies.

/// The MIME type an embedded file is served with, by extension. `None` fails the build: every
/// shipped file type is listed here on purpose (no guessing, no `nosniff` surprises).
pub fn mime_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webmanifest" => "application/manifest+json",
        "json" => "application/json",
        "bin" => "application/octet-stream",
        "woff2" => "font/woff2",
        _ => return None,
    })
}

/// Whether a path relative to `web/` (with `/` separators) stays out of the binary: tests,
/// deno's config and lock files, dotfiles, READMEs, and the JSDoc-only type modules
/// (`js/types*.js`: read by `deno check`, never loaded by the page).
pub fn skipped(rel: &str) -> bool {
    let mut parts = rel.split('/').peekable();
    while let Some(p) = parts.next() {
        if p.starts_with('.') {
            return true;
        }
        let last = parts.peek().is_none();
        if !last && p == "test" {
            return true;
        }
        if last
            && (p.ends_with("_test.js")
                || p.ends_with(".test.js")
                || p == "deno.json"
                || p == "deno.lock"
                || p.starts_with("README")
                || p == "types.js"
                || (p.starts_with("types-") && p.ends_with(".js")))
        {
            return true;
        }
    }
    false
}

/// FNV-1a 64 over `(path, NUL, bytes)` for every file, in the order given (sorted by path), as
/// 16 lowercase hex digits: the asset VERSION.
pub fn version<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(PRIME);
        }
    };
    for (path, bytes) in files {
        feed(path.as_bytes());
        feed(&[0]);
        feed(bytes);
    }
    format!("{h:016x}")
}

/// Every embeddable file under `root` as `(relative path, absolute path)`, sorted by relative
/// path. Errors name the offending file (non-UTF-8 name, unknown extension).
pub fn collect(root: &std::path::Path) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .to_str()
                .ok_or_else(|| format!("{}: file name is not UTF-8", path.display()))?
                .replace('\\', "/");
            let ty = entry
                .file_type()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if ty.is_dir() {
                // A directory is skipped like a file inside it would be.
                if !skipped(&format!("{rel}/x.js")) {
                    dirs.push(path);
                }
                continue;
            }
            if skipped(&rel) {
                continue;
            }
            if mime_for(&rel).is_none() {
                return Err(format!(
                    "web UI file {rel}: unknown extension (add it to web_build::mime_for)"
                ));
            }
            out.push((rel, path));
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_by_extension() {
        assert_eq!(mime_for("index.html"), Some("text/html; charset=utf-8"));
        assert_eq!(
            mime_for("js/main.js"),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(mime_for("css/app.css"), Some("text/css; charset=utf-8"));
        assert_eq!(mime_for("icons/icon.svg"), Some("image/svg+xml"));
        assert_eq!(mime_for("icons/icon-192.png"), Some("image/png"));
        assert_eq!(
            mime_for("manifest.webmanifest"),
            Some("application/manifest+json")
        );
        assert_eq!(mime_for("a.json"), Some("application/json"));
        assert_eq!(
            mime_for("assets/world_110m.bin"),
            Some("application/octet-stream")
        );
        assert_eq!(mime_for("f.woff2"), Some("font/woff2"));
        assert_eq!(mime_for("x.exe"), None);
        assert_eq!(mime_for("Makefile"), None);
    }

    #[test]
    fn skip_rules() {
        for s in [
            "test/format.test.js",
            "test/assert-helpers.js",
            "js/x.test.js",
            "js/x_test.js",
            "deno.json",
            "deno.lock",
            ".eslintrc",
            "js/.hidden.js",
            "README.md",
            "js/README",
            "js/types.js",
            "js/types-stats.js",
        ] {
            assert!(skipped(s), "{s}");
        }
        for s in [
            "index.html",
            "js/main.js",
            "js/views/test-view.js",
            "js/typescale.js",
            "css/app.css",
        ] {
            assert!(!skipped(s), "{s}");
        }
    }

    #[test]
    fn version_is_deterministic_and_content_sensitive() {
        let a = version([("a.js", &b"x"[..]), ("b.js", &b"y"[..])]);
        assert_eq!(a, version([("a.js", &b"x"[..]), ("b.js", &b"y"[..])]));
        assert_eq!(a.len(), 16);
        assert_ne!(a, version([("a.js", &b"x"[..]), ("b.js", &b"z"[..])]));
        // The NUL separator: moving a byte between path and content changes the hash.
        assert_ne!(version([("ab", &b"c"[..])]), version([("a", &b"bc"[..])]));
        // FNV-1a 64 of the empty input is the offset basis.
        assert_eq!(version([]), "cbf29ce484222325");
    }
}

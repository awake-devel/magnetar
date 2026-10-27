//! IP address → (country, AS number, AS organisation) for the crawl map (D101, D104,
//! docs/CRAWLMAP.md), from the free DB-IP Lite databases (CC BY 4.0, attribution required).
//!
//! **Privacy rule (D101).** An address handed to [`Resolver::lookup`] is turned into a
//! [`GeoCode`] and dropped: nothing here keeps, logs, hashes or returns an address, and the
//! databases are only read. Callers count codes, never addresses.
//!
//! The server (or worker) downloads `dbip-country-lite-YYYY-MM.mmdb.gz` and
//! `dbip-asn-lite-YYYY-MM.mmdb.gz` from `download.db-ip.com/free/` into its state dir on first
//! start and again when a newer monthly release is due ([`refresh`]); `MAGNETAR_GEOIP=off`
//! disables everything, `country` skips the ASN database, `MAGNETAR_GEOIP_DIR` and
//! `MAGNETAR_GEOIP_DOWNLOAD=false` use files placed there by hand (offline). Without a
//! database the map reports itself unavailable and nothing else changes.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use maxminddb::{Reader, geoip2};
use serde::{Deserialize, Serialize};

pub const COUNTRY_FILE: &str = "dbip-country-lite.mmdb";
pub const ASN_FILE: &str = "dbip-asn-lite.mmdb";
pub const DB_NAME_COUNTRY: &str = "DB-IP IP to Country Lite";
pub const DB_NAME_ASN: &str = "DB-IP IP to ASN Lite";
pub const LICENCE: &str = "CC BY 4.0";
pub const DBIP_URL: &str = "https://db-ip.com";
/// Shown with every answer that carries geo counts (the licence asks for a link back).
pub const ATTRIBUTION: &str = "IP Geolocation by DB-IP (https://db-ip.com), CC BY 4.0";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Largest compressed / decompressed download accepted (the 2026-10 files: 4–5 / 8–10 MB).
const MAX_GZ: usize = 64 << 20;
const MAX_MMDB: usize = 256 << 20;

/// `MAGNETAR_GEOIP`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeoMode {
    /// Country and network (both databases, ~18 MB in memory).
    #[default]
    On,
    /// Country only (~8 MB; for small machines such as the RPI worker).
    Country,
    /// No geo database, no counting; the map says `disabled`.
    Off,
}

/// Country and network of an address. `cc` is ISO 3166 alpha-2 in upper case, `[0, 0]` when
/// unknown; `asn` 0 when unknown. This is what is counted and stored, never the address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GeoCode {
    pub cc: [u8; 2],
    pub asn: u32,
}

impl GeoCode {
    pub const UNKNOWN: GeoCode = GeoCode { cc: [0, 0], asn: 0 };

    /// From stored / wire parts; anything but two ASCII letters is "unknown".
    pub fn from_parts(cc: Option<&str>, asn: Option<u32>) -> Self {
        let cc = match cc.map(str::as_bytes) {
            Some([a, b]) if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => {
                [a.to_ascii_uppercase(), b.to_ascii_uppercase()]
            }
            _ => [0, 0],
        };
        GeoCode {
            cc,
            asn: asn.unwrap_or(0),
        }
    }

    /// The country code, None when unknown.
    pub fn country(&self) -> Option<&str> {
        (self.cc != [0, 0])
            .then(|| std::str::from_utf8(&self.cc).ok())
            .flatten()
    }

    /// Stored form of the country: `''` when unknown (a key column, never NULL).
    pub fn cc_text(&self) -> String {
        self.country().unwrap_or("").to_string()
    }

    pub fn asn(&self) -> Option<u32> {
        (self.asn != 0).then_some(self.asn)
    }
}

/// What a lookup gives: the code, and the AS organisation when asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GeoHit {
    pub code: GeoCode,
    pub org: Option<String>,
}

/// Turns an address into a [`GeoCode`] (and the AS name). Implementations keep nothing.
pub trait Resolver: Send + Sync {
    /// `want_org`: also return the AS organisation (an allocation; callers ask once per AS).
    fn lookup(&self, ip: IpAddr, want_org: bool) -> GeoHit;
    fn info(&self) -> magnetar_core::api::GeoDbInfo;
}

/// The DB-IP Lite databases, read into memory.
pub struct Mmdb {
    country: Reader<Vec<u8>>,
    asn: Option<Reader<Vec<u8>>>,
}

impl std::fmt::Debug for Mmdb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mmdb")
            .field("country_build", &self.country.metadata().build_epoch)
            .field("asn", &self.asn.as_ref().map(|r| r.metadata().build_epoch))
            .finish()
    }
}

/// Opens an MMDB of the expected kind (`Country` / `ASN` in its `database_type`).
fn open_kind(bytes: Vec<u8>, kind: &str) -> Result<Reader<Vec<u8>>, String> {
    let r = Reader::from_source(bytes).map_err(|e| format!("not an MMDB file: {e}"))?;
    let t = &r.metadata().database_type;
    if !t.to_ascii_lowercase().contains(&kind.to_ascii_lowercase()) {
        return Err(format!("database type {t:?} is not a {kind} database"));
    }
    Ok(r)
}

impl Mmdb {
    pub fn from_bytes(country: Vec<u8>, asn: Option<Vec<u8>>) -> Result<Self, String> {
        Ok(Self {
            country: open_kind(country, "country")?,
            asn: asn.map(|b| open_kind(b, "asn")).transpose()?,
        })
    }

    /// Opens `<dir>/dbip-country-lite.mmdb` (and the ASN file unless `mode` is `Country`).
    pub fn open(dir: &Path, mode: GeoMode) -> Result<Self, String> {
        let read = |name: &str| {
            std::fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()))
        };
        let asn = match mode {
            GeoMode::On => Some(read(ASN_FILE)?),
            _ => None,
        };
        Self::from_bytes(read(COUNTRY_FILE)?, asn)
    }

    pub fn country_build(&self) -> i64 {
        self.country.metadata().build_epoch as i64
    }

    pub fn asn_build(&self) -> Option<i64> {
        self.asn.as_ref().map(|r| r.metadata().build_epoch as i64)
    }
}

impl Resolver for Mmdb {
    fn lookup(&self, ip: IpAddr, want_org: bool) -> GeoHit {
        let cc: Option<&str> = self
            .country
            .lookup(ip)
            .ok()
            .and_then(|r| {
                r.decode_path::<&str>(&maxminddb::path!["country", "iso_code"])
                    .ok()
            })
            .flatten();
        let (asn, org) = match &self.asn {
            Some(r) => match r
                .lookup(ip)
                .ok()
                .and_then(|x| x.decode::<geoip2::Asn>().ok())
            {
                Some(Some(a)) => (
                    a.autonomous_system_number,
                    if want_org {
                        a.autonomous_system_organization.map(clean_org)
                    } else {
                        None
                    },
                ),
                _ => (None, None),
            },
            None => (None, None),
        };
        GeoHit {
            code: GeoCode::from_parts(cc, asn),
            org,
        }
    }

    fn info(&self) -> magnetar_core::api::GeoDbInfo {
        magnetar_core::api::GeoDbInfo {
            country_build: Some(self.country_build()),
            asn_build: self.asn_build(),
            name: if self.asn.is_some() {
                format!("{DB_NAME_COUNTRY} + {DB_NAME_ASN}")
            } else {
                DB_NAME_COUNTRY.into()
            },
            licence: LICENCE.into(),
            url: DBIP_URL.into(),
        }
    }
}

/// AS names are free text from the database: printable, at most 120 characters.
pub fn clean_org(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A fixed table for tests and offline fixtures: IPv4 prefixes → code and AS name.
#[derive(Clone, Debug, Default)]
pub struct TableResolver {
    pub rows: Vec<(std::net::Ipv4Addr, u8, GeoCode, &'static str)>,
}

impl Resolver for TableResolver {
    fn lookup(&self, ip: IpAddr, want_org: bool) -> GeoHit {
        let IpAddr::V4(v4) = ip else {
            return GeoHit::default();
        };
        let a = u32::from(v4);
        for (net, len, code, org) in &self.rows {
            let mask = if *len == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(*len))
            };
            if a & mask == u32::from(*net) & mask {
                return GeoHit {
                    code: *code,
                    org: (want_org && !org.is_empty()).then(|| org.to_string()),
                };
            }
        }
        GeoHit::default()
    }

    fn info(&self) -> magnetar_core::api::GeoDbInfo {
        magnetar_core::api::GeoDbInfo {
            name: "test table".into(),
            licence: LICENCE.into(),
            url: DBIP_URL.into(),
            ..Default::default()
        }
    }
}

// ---- download and monthly refresh

/// Where the files live and whether to fetch them.
#[derive(Clone, Debug)]
pub struct GeoConfig {
    pub mode: GeoMode,
    pub dir: PathBuf,
    pub download: bool,
    /// `https://download.db-ip.com/free` (tests point it elsewhere).
    pub base_url: String,
}

pub const DEFAULT_BASE_URL: &str = "https://download.db-ip.com/free";

/// `dbip-<kind>-lite-YYYY-MM.mmdb.gz` (kind `country` or `asn`), the URL format of
/// db-ip.com/db/download/ip-to-country-lite (checked 2026-10-27).
pub fn release_url(base: &str, kind: &str, year: i32, month: u32) -> String {
    format!(
        "{}/dbip-{kind}-lite-{year:04}-{month:02}.mmdb.gz",
        base.trim_end_matches('/')
    )
}

/// (year, month) of a unix time, UTC.
pub fn year_month(unix: i64) -> (i32, u32) {
    // Civil-from-days (Howard Hinnant), enough for a month.
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }) as i32, m as u32)
}

fn prev_month((y, m): (i32, u32)) -> (i32, u32) {
    if m == 1 { (y - 1, 12) } else { (y, m - 1) }
}

/// A database built in an earlier month than `now` is due for the new release (published on
/// the 1st; asked for from the 2nd).
pub fn is_stale(build_epoch: i64, now: i64) -> bool {
    let this = year_month(now);
    // From the 2nd: yesterday was in this month too.
    year_month(build_epoch) < this && year_month(now - 86_400) == this
}

async fn fetch_release(
    client: &reqwest::Client,
    url: &str,
    kind: &str,
) -> Result<Option<Vec<u8>>, String> {
    let resp = client
        .get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    if resp.content_length().is_some_and(|n| n as usize > MAX_GZ) {
        return Err(format!("{url}: too large"));
    }
    let gz = resp.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    if gz.len() > MAX_GZ {
        return Err(format!("{url}: too large"));
    }
    let mut out = Vec::new();
    {
        use std::io::Read;
        let mut d = flate2::read::GzDecoder::new(&gz[..]).take(MAX_MMDB as u64 + 1);
        d.read_to_end(&mut out)
            .map_err(|e| format!("{url}: gunzip: {e}"))?;
    }
    if out.len() > MAX_MMDB {
        return Err(format!("{url}: too large once decompressed"));
    }
    // Checked before it replaces anything.
    open_kind(out.clone(), kind).map_err(|e| format!("{url}: {e}"))?;
    Ok(Some(out))
}

/// Download the release of this month (or the previous one while this month's is not out)
/// into `<dir>/<file>`, atomically. Returns the build month it got.
async fn download(
    client: &reqwest::Client,
    cfg: &GeoConfig,
    kind: &str,
    file: &str,
    now: i64,
) -> Result<(), String> {
    let this = year_month(now);
    for (y, m) in [this, prev_month(this)] {
        let url = release_url(&cfg.base_url, kind, y, m);
        if let Some(bytes) = fetch_release(client, &url, kind).await? {
            std::fs::create_dir_all(&cfg.dir).map_err(|e| format!("{}: {e}", cfg.dir.display()))?;
            let tmp = cfg.dir.join(format!(".{file}.part"));
            std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, cfg.dir.join(file))
                .map_err(|e| format!("{}: {e}", cfg.dir.display()))?;
            tracing::info!("geo: downloaded {url} ({} bytes)", bytes.len());
            return Ok(());
        }
    }
    Err(format!(
        "no {kind} release found for {}-{:02} or the month before",
        this.0, this.1
    ))
}

/// Why the map has no database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Disabled,
    Missing,
    Invalid,
}

impl Unavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Unavailable::Disabled => "disabled",
            Unavailable::Missing => "missing",
            Unavailable::Invalid => "invalid",
        }
    }
}

/// Make sure the databases are there and current (downloading when allowed), then open them.
/// A failed refresh keeps the files already there.
pub async fn refresh(cfg: &GeoConfig, now: i64) -> Result<Mmdb, (Unavailable, String)> {
    if cfg.mode == GeoMode::Off {
        return Err((Unavailable::Disabled, "MAGNETAR_GEOIP=off".into()));
    }
    let mut kinds = vec![("country", COUNTRY_FILE)];
    if cfg.mode == GeoMode::On {
        kinds.push(("asn", ASN_FILE));
    }
    if cfg.download {
        let client = magnetar_core::http::client(DOWNLOAD_TIMEOUT)
            .map_err(|e| (Unavailable::Missing, e.to_string()))?;
        for (kind, file) in &kinds {
            let path = cfg.dir.join(file);
            let due = match std::fs::read(&path) {
                Ok(b) => match open_kind(b, kind) {
                    Ok(r) => is_stale(r.metadata().build_epoch as i64, now),
                    Err(_) => true,
                },
                Err(_) => true,
            };
            if due && let Err(e) = download(&client, cfg, kind, file, now).await {
                tracing::warn!("geo: {e}");
            }
        }
    }
    match Mmdb::open(&cfg.dir, cfg.mode) {
        Ok(db) => Ok(db),
        Err(e) => {
            let missing = kinds.iter().any(|(_, f)| !cfg.dir.join(f).exists());
            Err((
                if missing {
                    Unavailable::Missing
                } else {
                    Unavailable::Invalid
                },
                e,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_and_urls() {
        // 2026-10-27 12:00 UTC
        let t = 1_793_102_400;
        assert_eq!(year_month(t), (2026, 10));
        assert_eq!(year_month(1_767_225_600), (2026, 1)); // 2026-01-01 00:00
        assert_eq!(year_month(1_767_225_599), (2025, 12));
        assert_eq!(prev_month((2026, 1)), (2025, 12));
        assert_eq!(
            release_url(DEFAULT_BASE_URL, "country", 2026, 3),
            "https://download.db-ip.com/free/dbip-country-lite-2026-03.mmdb.gz"
        );
        // Built in August, it is late October: due. Built this month: not due.
        assert!(is_stale(1_788_000_000, t));
        assert!(!is_stale(1_790_900_000, t)); // 2026-10-01
        // On the 1st of a month, the old release is still kept (the new one may not be out).
        assert!(!is_stale(1_788_000_000, 1_790_812_800 + 3600)); // 2026-10-01 01:00
    }

    #[test]
    fn codes() {
        let c = GeoCode::from_parts(Some("fr"), Some(3215));
        assert_eq!(c.country(), Some("FR"));
        assert_eq!(c.asn(), Some(3215));
        assert_eq!(GeoCode::from_parts(Some("ZZZ"), None), GeoCode::UNKNOWN);
        assert_eq!(GeoCode::from_parts(Some("1."), None).country(), None);
        assert_eq!(clean_org("Orange\u{0}  S.A.\n"), "Orange  S.A.");
    }

    #[test]
    fn table_resolver() {
        let t = TableResolver {
            rows: vec![(
                "203.0.113.0".parse().unwrap(),
                24,
                GeoCode::from_parts(Some("NZ"), Some(64500)),
                "Example AS",
            )],
        };
        let h = t.lookup("203.0.113.9".parse().unwrap(), true);
        assert_eq!(h.code.country(), Some("NZ"));
        assert_eq!(h.org.as_deref(), Some("Example AS"));
        assert_eq!(
            t.lookup("198.51.100.1".parse().unwrap(), true),
            GeoHit::default()
        );
    }

    /// The real databases, when present (`MAGNETAR_TEST_GEOIP_DIR`): known addresses resolve.
    #[test]
    fn real_databases_when_present() {
        let Some(dir) = std::env::var_os("MAGNETAR_TEST_GEOIP_DIR") else {
            return;
        };
        let db = Mmdb::open(Path::new(&dir), GeoMode::On).expect("open");
        let h = db.lookup("1.1.1.1".parse().unwrap(), true);
        assert_eq!(h.code.asn(), Some(13335));
        assert!(h.org.unwrap().to_lowercase().contains("cloudflare"));
        assert!(h.code.country().is_some());
        let h = db.lookup("2a01:e0a::1".parse().unwrap(), false);
        assert_eq!(h.code.country(), Some("FR"));
    }
}

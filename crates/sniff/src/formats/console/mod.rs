//! Console packages, disc/cartridge images and folder-dump metadata files.
//!
//! Every parser fills one [`GameInfo`] with the same id schema: `platform`, `title_id` (the
//! platform's own id), `content_id` (Sony), `db_id` (the key a game database such as GameTDB
//! uses, checked by [`db_id_valid`]), `region`, `version` and `content` (game / patch / DLC).
//! Ids that sit inside encrypted data are left `None` and `encrypted` says so.
//!
//! Only the PS5 FIH path is checked against real files (two local packages, possibly
//! fake-signed). Every other layout is written from format documentation and is UNVERIFIED until
//! a real sample passes through it.

use serde::Serialize;

use crate::types::{Field, Hit, Value};

pub(crate) mod iso;
pub mod listing;
pub(crate) mod nintendo;
pub(crate) mod retro;
pub(crate) mod sony;
pub(crate) mod xbox;

/// Rust checks of this area, run by the engine with the other formats.
pub(crate) static CHECKS: &[(&str, crate::formats::Check)] = &[
    ("ps5_fih", sony::ps5_fih),
    ("ps4_cnt", sony::ps4_cnt),
    ("ps3_pkg", sony::ps3_pkg),
    ("param_sfo", sony::param_sfo_file),
    ("pbp_sfo", sony::psp_pbp),
    ("param_json", sony::param_json_file),
    ("ps3_disc", sony::ps3_disc),
    ("sony_iso", iso::sony_iso),
    ("psp_cso", iso::psp_cso),
    ("pfs0", nintendo::pfs0),
    ("xci", nintendo::xci),
    ("ncsd", nintendo::ncsd),
    ("ncch", nintendo::ncch),
    ("cia", nintendo::cia),
    ("wii_gc_raw", nintendo::wii_gc_raw),
    ("wbfs", nintendo::wbfs),
    ("wia_rvz", nintendo::wia_rvz),
    ("gcz", nintendo::gcz),
    ("ciso", nintendo::ciso),
    ("wii_wad", nintendo::wii_wad),
    ("wiiu_wud", nintendo::wiiu_wud),
    ("wiiu_meta", nintendo::wiiu_meta),
    ("nds", nintendo::nds),
    ("dol", nintendo::dol),
    ("nro", nintendo::nro),
    ("gba", retro::gba),
    ("gb", retro::gb),
    ("n64", retro::n64),
    ("nes", retro::nes),
    ("snes", retro::snes),
    ("stfs", xbox::stfs),
    ("xvd", xbox::xvd),
    ("xbe", xbox::xbe),
    ("xex", xbox::xex),
    ("xiso", xbox::xiso),
    ("xdvdfs_probe", xbox::zero_head_xdvdfs),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Ps5,
    Ps4,
    Ps3,
    Psp,
    PsVita,
    /// A `\x7FPKG` of type 2 or a PARAM.SFO whose title ID did not say which.
    PspOrVita,
    /// ISO 9660 with system identifier "PLAYSTATION": PS1 and PS2 share it.
    Ps1OrPs2,
    Ps1,
    Ps2,
    Switch,
    Xbox,
    Xbox360,
    XboxOne,
    Wii,
    WiiU,
    GameCube,
    /// A container that names neither (GCZ without its sub-type, ...).
    WiiOrGameCube,
    N3ds,
    Nds,
    Gba,
    Gb,
    Gbc,
    Nes,
    /// Famicom Disk System.
    Fds,
    Snes,
    N64,
    MasterSystem,
    GameGear,
    Genesis,
    Sega32x,
    SegaCd,
    Saturn,
    Dreamcast,
    Lynx,
    Atari7800,
    NeoGeoPocket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// PS5 FIH / PS4 CNT / PS3 PKG / Switch NSP / 3DS CIA / Wii WAD / Xbox STFS or XVD.
    Package,
    /// A raw disc or cartridge image (ISO, XCI, CCI, WUD, NDS, ...).
    Image,
    /// A compressed or re-blocked image (CSO, NSZ, XCZ, WBFS, RVZ, WIA, GCZ, CISO, WUX).
    CompressedImage,
    /// A loose metadata file from a folder dump (PARAM.SFO, param.json, meta.xml, default.xbe).
    Metadata,
}

/// What the package or image holds, when the format says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    Game,
    /// An update / patch (PS4 `gp`, Switch `...800`, 3DS `0004000E`, Wii U `0005000E`).
    Patch,
    /// Additional content (PS4 `ac`, Switch AOC, 3DS `0004008C`, Wii U `0005000C`).
    Dlc,
    Demo,
    /// A non-game application (media apps, tools, Wii channels).
    App,
    Theme,
    /// A licence / entitlement only (PS3 PKG content type 0x0B, PS4 `AL`).
    License,
    /// System software or a firmware update partition.
    System,
}

/// Console package / image details. Fields stay `None` when the bytes holding them were not
/// fetched or could not be read (encrypted, inconsistent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GameInfo {
    pub platform: Platform,
    pub kind: Kind,
    /// Short format name: "PS5 FIH", "PS4 PKG", "NSP", "RVZ", ...
    pub format: &'static str,
    /// Sony content ID (`EP9000-PPSA13427_00-HORIZONREMASTER1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// The platform's own id: PPSA12544, CUSA01021, BLES00932, SLES12345, ULES01234 (Sony
    /// serials, no dash), 0100ABCD12340000 (Switch / 3DS / Wii U 16 hex digits), RMCE01 (Wii /
    /// GameCube disc ID), AMCP (DS / GBA / N64 game code), 4D530004 (Xbox, 8 hex digits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_id: Option<String>,
    /// The key of a game database for this platform, in that database's format (see
    /// [`db_id_valid`]): GameTDB ids for Nintendo (Wii / GameCube 6 characters, Wii U 6 or 4,
    /// DS / 3DS 4, Switch the 5-character product code), the serial for Sony (GameTDB PS3 uses
    /// BLES00680), the 8-digit title ID for Xbox. `None` when the id is not readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_id: Option<String>,
    /// Printed product code: `CTR-P-ABCE`, `WUP-P-AAFE`, `HAC-P-AAAAA`, `GM 00001009-00`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    /// Lowest system software, as the platform writes it ("9.00").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_fw: Option<String>,
    /// SDK the title was built with (PS5 param.json `sdkVersion`, as "10.00").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk: Option<String>,
    /// From the content ID service prefix, the serial or the disc ID region letter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<&'static str>,
    /// Publisher / maker code (Nintendo two-character licensee code, "01" = Nintendo).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maker: Option<String>,
    /// PS5: FIH byte 5 is 0x00 (debug / fake-signed) or 0x80 (retail). PS3: revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<bool>,
    /// `Some(true)` when the ids that matter sit in encrypted data this crate cannot read
    /// (Switch NCA, XCI, 3DS CCI with encrypted ExeFS, Xbox One XVD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    /// Number of localised title languages (PS5 param.json).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<u32>,
}

impl GameInfo {
    pub fn new(platform: Platform, kind: Kind, format: &'static str) -> GameInfo {
        GameInfo {
            platform,
            kind,
            format,
            content_id: None,
            title_id: None,
            db_id: None,
            product_code: None,
            title: None,
            version: None,
            content: None,
            required_fw: None,
            sdk: None,
            region: None,
            maker: None,
            debug: None,
            encrypted: None,
            languages: None,
        }
    }

    /// Sets the content ID and, when unset, the title ID and region it implies.
    pub(crate) fn set_content_id(&mut self, cid: String) {
        if self.title_id.is_none() {
            self.title_id = cid.get(7..16).map(str::to_owned);
        }
        if self.region.is_none() {
            self.region = sony::sony_region(&cid);
        }
        self.content_id = Some(cid);
    }

    /// Fills `db_id` (and a missing region) from the other ids once a parser is done. Ids that
    /// fail the platform's format are not used as database keys.
    pub(crate) fn finish(&mut self) {
        if self
            .db_id
            .as_deref()
            .is_some_and(|d| !db_id_valid(self.platform, d))
        {
            self.db_id = None;
        }
        if self.db_id.is_none() {
            self.db_id = derive_db_id(self);
        }
        if self.region.is_none() && is_sony(self.platform) {
            self.region = self.title_id.as_deref().and_then(sony::serial_region);
        }
    }
}

fn is_sony(p: Platform) -> bool {
    use Platform::*;
    matches!(
        p,
        Ps5 | Ps4 | Ps3 | Psp | PsVita | PspOrVita | Ps1OrPs2 | Ps1 | Ps2
    )
}

/// The game code of a retail Nintendo product code (`CTR-P-ABCE`, `KTR-N-ABCE`,
/// `HAC-P-AAAAA`, `WUP-P-AAFE`): three dash-separated parts, a 3-letter system, a one-letter
/// media / distribution class and a 4 or 5 character code. Homebrew codes (`CTR-HB-CKPT`,
/// `ftpd`) give none: they are not GameTDB keys and their letters say nothing about a region.
pub(crate) fn retail_code(code: &str) -> Option<&str> {
    let mut parts = code.trim().split('-');
    let (sys, class, game) = (parts.next()?, parts.next()?, parts.next()?);
    let ok = parts.next().is_none()
        && sys.len() == 3
        && sys.bytes().all(|b| b.is_ascii_uppercase())
        && class.len() == 1
        && class.bytes().all(|b| b.is_ascii_uppercase())
        && (4..=5).contains(&game.len())
        && game
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    ok.then_some(game)
}

fn derive_db_id(g: &GameInfo) -> Option<String> {
    use Platform::*;
    let tid = g.title_id.as_deref();
    let code = g.product_code.as_deref();
    let cand: Option<String> = match g.platform {
        // GameTDB 3DS / DS ids are the 4-character game code: the tail of CTR-P-ABCE.
        N3ds | Nds => code
            .and_then(retail_code)
            .map(str::to_owned)
            .or_else(|| tid.filter(|t| t.len() == 4).map(str::to_owned)),
        // Switch: the 5-character tail of HAC-P-AAAAA (only from readable metadata).
        Switch => code.and_then(retail_code).map(str::to_owned),
        // Wii U: WUP-P-AAFE plus the two-character maker code = GameTDB AAFE01.
        WiiU => code.and_then(retail_code).map(|c| match &g.maker {
            Some(m) if m.len() == 2 => format!("{c}{m}"),
            _ => c.to_owned(),
        }),
        _ => tid.map(str::to_owned),
    };
    cand.filter(|c| db_id_valid(g.platform, c))
}

/// Whether `id` has the shape of a database key for `platform` (see [`GameInfo::db_id`]):
///
/// | platform | format | example |
/// |---|---|---|
/// | PS1-PS5, PSP, Vita | 4 letters + 5 digits | `BLES00680`, `CUSA00207`, `PPSA12544`, `SLES12345` |
/// | Wii, GameCube | 6 upper-case letters / digits | `RMCE01`, `GALE01` |
/// | Wii (WiiWare / VC) | 4 | `WAAE` |
/// | Wii U | 6, or 4 for eShop titles | `AAFE01`, `AA2E` |
/// | DS, 3DS, GBA, N64, SNES | 4 | `AMCP`, `ABCE`, `BPEE`, `NSME` |
/// | Switch | 5 (product code tail) | `AAAAA` |
/// | Xbox, Xbox 360, Xbox One | 8 hex digits | `4D530004` |
pub fn db_id_valid(platform: Platform, id: &str) -> bool {
    use Platform::*;
    let b = id.as_bytes();
    let up_alnum = |c: &u8| c.is_ascii_uppercase() || c.is_ascii_digit();
    match platform {
        Ps5 | Ps4 | Ps3 | Psp | PsVita | PspOrVita | Ps1OrPs2 | Ps1 | Ps2 => {
            b.len() == 9
                && b.iter().take(4).all(u8::is_ascii_uppercase)
                && b.iter().skip(4).all(u8::is_ascii_digit)
        }
        Wii | GameCube | WiiOrGameCube => {
            (b.len() == 6 || (platform == Wii && b.len() == 4)) && b.iter().all(up_alnum)
        }
        WiiU => (b.len() == 6 || b.len() == 4) && b.iter().all(up_alnum),
        Nds | N3ds | Gba | N64 | Snes | Gbc => b.len() == 4 && b.iter().all(up_alnum),
        Switch => b.len() == 5 && b.iter().all(up_alnum),
        Xbox | Xbox360 | XboxOne => b.len() == 8 && b.iter().all(u8::is_ascii_hexdigit),
        _ => !b.is_empty() && b.len() <= 32 && b.iter().all(|c| c.is_ascii_graphic() || *c == b' '),
    }
}

/// [`GameInfo`] for labels produced by signature rules (no parser of their own), filled from
/// the rule's captured fields (`title`, `title_id`, `product_code`, `version`, `region_code`,
/// `maker`).
pub(crate) fn game_from_label(label: &str, fields: &[Field]) -> Option<GameInfo> {
    use Kind::*;
    use Platform::*;
    let (p, k, f) = match label {
        "game.nes.rom" => (Nes, Image, "NES"),
        "game.fds.disk" => (Fds, Image, "FDS"),
        "game.n64.rom" => (N64, Image, "N64"),
        "game.gb.rom" => (Gb, Image, "GB"),
        "game.snes.rom" => (Snes, Image, "SNES"),
        "game.switch.ncz" => (Switch, CompressedImage, "NCZ"),
        "game.3ds.cxi" => (N3ds, Image, "CXI"),
        "game.psp.pbp" => (Psp, Metadata, "PBP"),
        "game.ps1.iso" => (Ps1, Image, "PS1 ISO"),
        "game.ps1.bin" => (Ps1, Image, "PS1 BIN"),
        "game.ps2.iso" => (Ps2, Image, "PS2 ISO"),
        "game.psp.iso" => (Psp, Image, "PSP ISO"),
        "game.ps3.iso" => (Ps3, Image, "PS3 ISO"),
        "game.genesis.rom" => (Genesis, Image, "Mega Drive"),
        "game.32x.rom" => (Sega32x, Image, "32X"),
        "game.sms.rom" => (MasterSystem, Image, "SMS"),
        "game.gg.rom" => (GameGear, Image, "Game Gear"),
        "game.segacd.iso" => (SegaCd, Image, "Mega-CD"),
        "game.saturn.iso" => (Saturn, Image, "Saturn"),
        "game.dreamcast.iso" => (Dreamcast, Image, "Dreamcast"),
        "game.lynx.rom" => (Lynx, Image, "LNX"),
        "game.a7800.rom" => (Atari7800, Image, "A78"),
        "game.ngp.rom" => (NeoGeoPocket, Image, "NGP"),
        _ => return None,
    };
    let mut g = GameInfo::new(p, k, f);
    let get = |key: &str| {
        fields
            .iter()
            .find(|x| x.key == key)
            .and_then(|x| match &x.value {
                Value::Str(s) => {
                    let t = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
                    (!t.is_empty()).then(|| t.to_owned())
                }
                Value::U64(v) => Some(v.to_string()),
                _ => None,
            })
    };
    g.title = get("title");
    g.title_id = get("title_id");
    g.product_code = get("product_code");
    g.version = get("version");
    g.maker = get("maker");
    if let Some(r) = get("region_code") {
        g.region = sega_region(&r);
    }
    g.finish();
    Some(g)
}

/// Sega header region field ("JUE", "U", "E", or the Saturn "JTUBKAEL" letters).
fn sega_region(r: &str) -> Option<&'static str> {
    let has = |c: char| r.contains(c);
    let n = ['J', 'U', 'E'].iter().filter(|c| has(**c)).count();
    Some(match () {
        _ if n > 1 => "World",
        _ if has('J') => "Japan",
        _ if has('U') => "Americas",
        _ if has('E') => "Europe/PAL",
        _ => return None,
    })
}

/// Attaches `g` to a hit (helper for the parsers); derives the database id.
pub(crate) fn with_game(mut hit: Hit, mut g: GameInfo) -> Hit {
    g.finish();
    hit.game = Some(g);
    hit
}

/// Nintendo disc / cartridge region letter (4th character of the game code).
pub(crate) fn nintendo_region(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes().get(3)? {
        b'E' | b'N' => "Americas",
        b'P' | b'D' | b'F' | b'S' | b'I' | b'X' | b'Y' | b'U' | b'H' | b'V' | b'Z' | b'L'
        | b'M' | b'R' => "Europe/PAL",
        b'J' => "Japan",
        b'K' | b'Q' | b'T' => "Korea",
        b'W' => "Taiwan",
        b'C' => "China",
        b'A' => "World",
        _ => return None,
    })
}

/// Nintendo-style title ID kinds (Switch, 3DS, Wii U, Wii).
pub(crate) fn nintendo_content(platform: Platform, tid: u64) -> Option<Content> {
    let hi = (tid >> 32) as u32;
    Some(match platform {
        // Switch: base ...000, update ...800, AOC (base ^ 0x1000) + n; 01000000000000xx system.
        Platform::Switch => {
            if tid & !0xFFF == 0x0100_0000_0000_0000 {
                Content::System
            } else if tid & 0xFFF == 0x800 {
                Content::Patch
            } else if tid & 0x1000 != 0 {
                Content::Dlc
            } else if tid & 0xFFF == 0 {
                Content::Game
            } else {
                return None;
            }
        }
        Platform::N3ds => match hi {
            0x0004_0000 => Content::Game,
            0x0004_0002 => Content::Demo,
            0x0004_000E => Content::Patch,
            0x0004_008C => Content::Dlc,
            0x0004_0010 | 0x0004_001B | 0x0004_0030 | 0x0004_009B | 0x0004_00DB | 0x0004_0130
            | 0x0004_0138 => Content::System,
            _ => return None,
        },
        Platform::WiiU => match hi {
            0x0005_0000 => Content::Game,
            0x0005_0002 => Content::Demo,
            0x0005_000E => Content::Patch,
            0x0005_000C => Content::Dlc,
            0x0005_0010 | 0x0005_001B | 0x0005_0030 => Content::System,
            _ => return None,
        },
        Platform::Wii => match hi {
            0x0001_0000 => Content::Game,
            0x0001_0001 => Content::App,
            0x0001_0002 | 0x0001_0008 | 0x0000_0001 => Content::System,
            0x0001_0004 => Content::Game,
            0x0001_0005 => Content::Dlc,
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_id_formats() {
        assert!(db_id_valid(Platform::Ps3, "BLES00680"));
        assert!(!db_id_valid(Platform::Ps3, "BLES-00680"));
        assert!(db_id_valid(Platform::Wii, "RMCE01"));
        assert!(db_id_valid(Platform::Wii, "WAAE"));
        assert!(!db_id_valid(Platform::GameCube, "GALE"));
        assert!(db_id_valid(Platform::WiiU, "AAFE01"));
        assert!(db_id_valid(Platform::Nds, "AMCP"));
        assert!(db_id_valid(Platform::Switch, "AAAAA"));
        assert!(!db_id_valid(Platform::Switch, "0100ABCD12340000"));
        assert!(db_id_valid(Platform::Xbox, "4D530004"));
    }

    #[test]
    fn nintendo_title_kinds() {
        use Content::*;
        let s = |t| nintendo_content(Platform::Switch, t);
        assert_eq!(s(0x0100_ABCD_1234_0000), Some(Game));
        assert_eq!(s(0x0100_ABCD_1234_0800), Some(Patch));
        assert_eq!(s(0x0100_ABCD_1234_1001), Some(Dlc));
        assert_eq!(s(0x0100_0000_0000_0809), Some(System));
        assert_eq!(
            nintendo_content(Platform::N3ds, 0x0004_000E_0012_3400),
            Some(Patch)
        );
        assert_eq!(
            nintendo_content(Platform::WiiU, 0x0005_000C_1010_1D00),
            Some(Dlc)
        );
    }
}

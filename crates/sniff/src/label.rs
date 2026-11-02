//! Label registry: stable ids, MIME-ish types, descriptions and usual extensions.
//!
//! `legacy` is the prototype's label (`video/matroska`), kept so the evaluation corpora written
//! for the prototype can be scored unchanged.

/// One known label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LabelInfo {
    pub id: &'static str,
    pub legacy: &'static str,
    pub mime: &'static str,
    pub description: &'static str,
    /// Extensions the type is normally seen with (tie-breaks and mismatch flags only).
    pub extensions: &'static [&'static str],
}

macro_rules! l {
    ($id:literal, $legacy:literal, $mime:literal, $desc:literal, [$($e:literal),* $(,)?]) => {
        LabelInfo { id: $id, legacy: $legacy, mime: $mime, description: $desc, extensions: &[$($e),*] }
    };
}

/// Every label a built-in check or signature can produce.
pub static LABELS: &[LabelInfo] = &[
    // ---------------------------------------------------------------- video
    l!(
        "video.mkv",
        "video/matroska",
        "video/x-matroska",
        "Matroska video",
        ["mkv", "mk3d"]
    ),
    l!(
        "video.webm",
        "video/webm",
        "video/webm",
        "WebM video",
        ["webm", "mkv"]
    ),
    l!(
        "video.mp4",
        "video/mp4",
        "video/mp4",
        "MPEG-4 video",
        ["mp4", "m4v", "mov", "f4v"]
    ),
    l!(
        "video.mov",
        "video/quicktime",
        "video/quicktime",
        "QuickTime movie",
        ["mov", "qt", "mp4"]
    ),
    l!(
        "video.3gp",
        "video/3gpp",
        "video/3gpp",
        "3GPP video",
        ["3gp", "3g2"]
    ),
    l!(
        "video.avi",
        "video/avi",
        "video/x-msvideo",
        "AVI video",
        ["avi", "divx"]
    ),
    l!(
        "video.ts",
        "video/mpeg-ts",
        "video/mp2t",
        "MPEG transport stream",
        ["ts", "mts", "m2t", "trp", "tp"]
    ),
    l!(
        "video.m2ts",
        "video/m2ts",
        "video/mp2t",
        "Blu-ray BDAV transport stream",
        ["m2ts", "mts", "ts"]
    ),
    l!(
        "video.mpeg_ps",
        "video/mpeg-ps",
        "video/mpeg",
        "MPEG program stream",
        ["mpg", "mpeg", "vob", "m2p", "ps", "evo", "dat"]
    ),
    l!(
        "video.flv",
        "video/flv",
        "video/x-flv",
        "Flash video",
        ["flv"]
    ),
    l!(
        "video.ogv",
        "video/ogg",
        "video/ogg",
        "Ogg video",
        ["ogv", "ogg"]
    ),
    l!(
        "video.asf",
        "video/asf",
        "video/x-ms-asf",
        "ASF / WMV video",
        ["wmv", "asf", "wma"]
    ),
    l!(
        "video.nut",
        "video/nut",
        "video/x-nut",
        "NUT video",
        ["nut"]
    ),
    l!(
        "video.y4m",
        "video/y4m",
        "video/x-yuv4mpeg",
        "YUV4MPEG2 raw video",
        ["y4m"]
    ),
    l!(
        "video.ivf",
        "video/ivf",
        "video/x-ivf",
        "IVF (VP8/VP9/AV1) video",
        ["ivf"]
    ),
    l!(
        "video.h264",
        "video/h264-es",
        "video/h264",
        "H.264 elementary stream",
        ["h264", "264", "avc"]
    ),
    l!(
        "video.hevc",
        "video/hevc-es",
        "video/h265",
        "HEVC elementary stream",
        ["hevc", "h265", "265"]
    ),
    l!(
        "video.mpeg2_es",
        "video/mpeg2-es",
        "video/mpeg",
        "MPEG-2 video elementary stream",
        ["m2v", "mpv"]
    ),
    l!(
        "video.mpeg4_es",
        "video/mpeg4-es",
        "video/mp4v-es",
        "MPEG-4 part 2 elementary stream",
        ["m4v", "xvid"]
    ),
    l!(
        "video.av1_obu",
        "video/av1-obu",
        "video/av1",
        "AV1 OBU stream",
        ["obu", "av1"]
    ),
    l!(
        "video.mxf",
        "video/mxf",
        "application/mxf",
        "MXF video",
        ["mxf"]
    ),
    l!(
        "video.dv",
        "video/dv",
        "video/dv",
        "DV video",
        ["dv", "dif"]
    ),
    l!(
        "video.rm",
        "video/rm",
        "application/vnd.rn-realmedia",
        "RealMedia",
        ["rm", "rmvb", "ra"]
    ),
    l!(
        "video.bink",
        "video/bink",
        "video/x-bink",
        "Bink video",
        ["bik", "bk2"]
    ),
    l!(
        "video.smacker",
        "video/smacker",
        "video/x-smacker",
        "Smacker video",
        ["smk"]
    ),
    l!(
        "video.amv",
        "video/amv",
        "video/x-amv",
        "AMV video",
        ["amv"]
    ),
    // ---------------------------------------------------------------- audio
    l!(
        "audio.mka",
        "audio/matroska",
        "audio/x-matroska",
        "Matroska audio",
        ["mka", "mkv"]
    ),
    l!(
        "audio.m4a",
        "audio/mp4",
        "audio/mp4",
        "MPEG-4 audio",
        ["m4a", "m4b", "mp4", "aac"]
    ),
    l!(
        "audio.wma",
        "audio/asf",
        "audio/x-ms-wma",
        "ASF / WMA audio",
        ["wma", "asf", "wmv"]
    ),
    l!(
        "audio.mpeg",
        "audio/mpeg",
        "audio/mpeg",
        "MPEG audio (MP3/MP2)",
        ["mp3", "mp2", "mpa"]
    ),
    l!(
        "audio.flac",
        "audio/flac",
        "audio/flac",
        "FLAC audio",
        ["flac"]
    ),
    l!(
        "audio.ogg.vorbis",
        "audio/ogg-vorbis",
        "audio/ogg",
        "Ogg Vorbis audio",
        ["ogg", "oga"]
    ),
    l!(
        "audio.ogg.opus",
        "audio/ogg-opus",
        "audio/ogg",
        "Ogg Opus audio",
        ["opus", "ogg", "oga"]
    ),
    l!(
        "audio.ogg.flac",
        "audio/ogg-flac",
        "audio/ogg",
        "Ogg FLAC audio",
        ["oga", "ogg"]
    ),
    l!(
        "audio.ogg.speex",
        "audio/ogg-speex",
        "audio/ogg",
        "Ogg Speex audio",
        ["spx", "ogg"]
    ),
    l!(
        "audio.ogg",
        "audio/ogg",
        "audio/ogg",
        "Ogg audio",
        ["ogg", "oga"]
    ),
    l!("audio.wav", "audio/wav", "audio/wav", "WAVE audio", ["wav"]),
    l!(
        "audio.w64",
        "audio/w64",
        "audio/x-w64",
        "Sony Wave64 audio",
        ["w64"]
    ),
    l!(
        "audio.aiff",
        "audio/aiff",
        "audio/aiff",
        "AIFF audio",
        ["aiff", "aif", "aifc"]
    ),
    l!(
        "audio.aac",
        "audio/aac-adts",
        "audio/aac",
        "AAC (ADTS) audio",
        ["aac", "adts"]
    ),
    l!(
        "audio.ac3",
        "audio/ac3",
        "audio/ac3",
        "Dolby Digital (AC-3) audio",
        ["ac3"]
    ),
    l!(
        "audio.eac3",
        "audio/eac3",
        "audio/eac3",
        "Dolby Digital Plus (E-AC-3) audio",
        ["eac3", "ec3"]
    ),
    l!(
        "audio.dts",
        "audio/dts",
        "audio/vnd.dts",
        "DTS audio",
        ["dts", "dtshd"]
    ),
    l!(
        "audio.truehd",
        "audio/truehd",
        "audio/vnd.dolby.mlp",
        "Dolby TrueHD audio",
        ["thd", "truehd", "mlp"]
    ),
    l!(
        "audio.mlp",
        "audio/mlp",
        "audio/vnd.dolby.mlp",
        "MLP audio",
        ["mlp"]
    ),
    l!(
        "audio.wavpack",
        "audio/wavpack",
        "audio/x-wavpack",
        "WavPack audio",
        ["wv"]
    ),
    l!(
        "audio.tta",
        "audio/tta",
        "audio/x-tta",
        "True Audio",
        ["tta"]
    ),
    l!(
        "audio.caf",
        "audio/caf",
        "audio/x-caf",
        "Core Audio Format",
        ["caf"]
    ),
    l!(
        "audio.au",
        "audio/au",
        "audio/basic",
        "Sun/NeXT audio",
        ["au", "snd"]
    ),
    l!("audio.amr", "audio/amr", "audio/amr", "AMR audio", ["amr"]),
    l!(
        "audio.voc",
        "audio/voc",
        "audio/x-voc",
        "Creative Voice audio",
        ["voc"]
    ),
    l!(
        "audio.ape",
        "audio/ape",
        "audio/x-ape",
        "Monkey's Audio",
        ["ape"]
    ),
    l!(
        "audio.midi",
        "audio/midi",
        "audio/midi",
        "MIDI",
        ["mid", "midi", "rmi"]
    ),
    l!(
        "audio.optimfrog",
        "audio/optimfrog",
        "audio/x-optimfrog",
        "OptimFROG audio",
        ["ofr"]
    ),
    l!(
        "audio.dsf",
        "audio/dsf",
        "audio/x-dsf",
        "DSD stream file",
        ["dsf"]
    ),
    l!(
        "audio.dff",
        "audio/dff",
        "audio/x-dff",
        "DSDIFF audio",
        ["dff"]
    ),
    l!(
        "audio.qcp",
        "audio/qcp",
        "audio/qcelp",
        "QCP audio",
        ["qcp"]
    ),
    // ---------------------------------------------------------------- images
    l!("image.png", "image/png", "image/png", "PNG image", ["png"]),
    l!(
        "image.apng",
        "image/apng",
        "image/apng",
        "Animated PNG",
        ["apng", "png"]
    ),
    l!(
        "image.jpeg",
        "image/jpeg",
        "image/jpeg",
        "JPEG image",
        ["jpg", "jpeg", "jpe", "jfif"]
    ),
    l!("image.gif", "image/gif", "image/gif", "GIF image", ["gif"]),
    l!(
        "image.webp",
        "image/webp",
        "image/webp",
        "WebP image",
        ["webp"]
    ),
    l!(
        "image.bmp",
        "image/bmp",
        "image/bmp",
        "BMP image",
        ["bmp", "dib"]
    ),
    l!(
        "image.tiff",
        "image/tiff",
        "image/tiff",
        "TIFF image",
        ["tif", "tiff"]
    ),
    l!(
        "image.ico",
        "image/ico",
        "image/vnd.microsoft.icon",
        "Windows icon",
        ["ico", "cur"]
    ),
    l!(
        "image.psd",
        "image/psd",
        "image/vnd.adobe.photoshop",
        "Photoshop image",
        ["psd", "psb"]
    ),
    l!(
        "image.tga",
        "image/tga",
        "image/x-tga",
        "Targa image",
        ["tga"]
    ),
    l!(
        "image.pcx",
        "image/pcx",
        "image/x-pcx",
        "PCX image",
        ["pcx"]
    ),
    l!(
        "image.xpm",
        "image/xpm",
        "image/x-xpixmap",
        "X pixmap",
        ["xpm"]
    ),
    l!(
        "image.avif",
        "image/avif",
        "image/avif",
        "AVIF image",
        ["avif"]
    ),
    l!(
        "image.heic",
        "image/heic",
        "image/heic",
        "HEIF image",
        ["heic", "heif"]
    ),
    l!(
        "image.jxl",
        "image/jxl",
        "image/jxl",
        "JPEG XL image",
        ["jxl"]
    ),
    l!(
        "image.jp2",
        "image/jp2",
        "image/jp2",
        "JPEG 2000 image",
        ["jp2", "j2k", "jpx"]
    ),
    l!("image.qoi", "image/qoi", "image/qoi", "QOI image", ["qoi"]),
    l!(
        "image.exr",
        "image/exr",
        "image/x-exr",
        "OpenEXR image",
        ["exr"]
    ),
    l!(
        "image.dds",
        "image/dds",
        "image/vnd.ms-dds",
        "DirectDraw Surface",
        ["dds"]
    ),
    l!(
        "image.pnm",
        "image/pnm",
        "image/x-portable-anymap",
        "Netpbm image",
        ["pbm", "pgm", "ppm", "pnm"]
    ),
    l!(
        "image.svg",
        "image/svg",
        "image/svg+xml",
        "SVG image",
        ["svg"]
    ),
    l!(
        "image.ani",
        "image/ani",
        "application/x-navi-animation",
        "Windows animated cursor",
        ["ani"]
    ),
    l!(
        "image.raw.cr2",
        "image/raw-cr2",
        "image/x-canon-cr2",
        "Canon CR2 raw image",
        ["cr2"]
    ),
    l!(
        "image.raw.cr3",
        "image/raw-cr3",
        "image/x-canon-cr3",
        "Canon CR3 raw image",
        ["cr3"]
    ),
    l!(
        "image.raw.raf",
        "image/raw-raf",
        "image/x-fuji-raf",
        "Fujifilm RAF raw image",
        ["raf"]
    ),
    // ---------------------------------------------------------------- fonts
    l!("font.ttf", "font/ttf", "font/ttf", "TrueType font", ["ttf"]),
    l!("font.otf", "font/otf", "font/otf", "OpenType font", ["otf"]),
    l!(
        "font.ttc",
        "font/ttc",
        "font/collection",
        "TrueType collection",
        ["ttc"]
    ),
    l!("font.woff", "font/woff", "font/woff", "WOFF font", ["woff"]),
    l!(
        "font.woff2",
        "font/woff2",
        "font/woff2",
        "WOFF2 font",
        ["woff2"]
    ),
    l!(
        "font.type1",
        "font/type1",
        "application/x-font-type1",
        "PostScript Type 1 font",
        ["pfb", "pfa"]
    ),
    // ---------------------------------------------------------------- documents
    l!(
        "doc.pdf",
        "doc/pdf",
        "application/pdf",
        "PDF document",
        ["pdf"]
    ),
    l!(
        "doc.epub",
        "doc/epub",
        "application/epub+zip",
        "EPUB e-book",
        ["epub"]
    ),
    l!(
        "doc.mobi",
        "doc/mobi",
        "application/x-mobipocket-ebook",
        "Mobipocket / Kindle e-book",
        ["mobi", "azw", "azw3", "prc"]
    ),
    l!(
        "doc.palmdoc",
        "doc/palmdoc",
        "application/vnd.palm",
        "PalmDOC e-book",
        ["pdb", "prc"]
    ),
    l!(
        "doc.ereader",
        "doc/ereader",
        "application/vnd.palm",
        "eReader e-book",
        ["pdb"]
    ),
    l!(
        "doc.topaz",
        "doc/topaz",
        "application/x-topaz-ebook",
        "Kindle Topaz e-book",
        ["tpz", "azw1"]
    ),
    l!(
        "doc.lit",
        "doc/lit",
        "application/x-ms-reader",
        "Microsoft Reader e-book",
        ["lit"]
    ),
    l!(
        "doc.djvu",
        "doc/djvu",
        "image/vnd.djvu",
        "DjVu document",
        ["djvu", "djv"]
    ),
    l!(
        "doc.chm",
        "doc/chm",
        "application/vnd.ms-htmlhelp",
        "Compiled HTML help",
        ["chm"]
    ),
    l!(
        "doc.fb2",
        "doc/fb2",
        "application/x-fictionbook+xml",
        "FictionBook e-book",
        ["fb2"]
    ),
    l!(
        "doc.rtf",
        "doc/rtf",
        "application/rtf",
        "Rich Text document",
        ["rtf"]
    ),
    l!(
        "doc.postscript",
        "doc/postscript",
        "application/postscript",
        "PostScript document",
        ["ps", "eps"]
    ),
    l!(
        "doc.docx",
        "doc/docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "Word document",
        ["docx"]
    ),
    l!(
        "doc.xlsx",
        "doc/xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "Excel workbook",
        ["xlsx"]
    ),
    l!(
        "doc.pptx",
        "doc/pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "PowerPoint presentation",
        ["pptx"]
    ),
    l!(
        "doc.doc",
        "doc/doc",
        "application/msword",
        "Word 97-2003 document",
        ["doc"]
    ),
    l!(
        "doc.xls",
        "doc/xls",
        "application/vnd.ms-excel",
        "Excel 97-2003 workbook",
        ["xls"]
    ),
    l!(
        "doc.ppt",
        "doc/ppt",
        "application/vnd.ms-powerpoint",
        "PowerPoint 97-2003 presentation",
        ["ppt"]
    ),
    l!(
        "doc.odf",
        "doc/odf",
        "application/vnd.oasis.opendocument",
        "OpenDocument file",
        ["odt", "ods", "odp"]
    ),
    l!(
        "doc.cbz",
        "doc/cbz",
        "application/vnd.comicbook+zip",
        "Comic book (ZIP)",
        ["cbz"]
    ),
    l!(
        "doc.cbr",
        "doc/cbr",
        "application/vnd.comicbook-rar",
        "Comic book (RAR)",
        ["cbr"]
    ),
    l!(
        "doc.cb7",
        "doc/cb7",
        "application/x-cb7",
        "Comic book (7z)",
        ["cb7"]
    ),
    // ---------------------------------------------------------------- archives
    l!(
        "archive.zip",
        "archive/zip",
        "application/zip",
        "ZIP archive",
        ["zip", "001"]
    ),
    l!(
        "archive.rar4",
        "archive/rar4",
        "application/vnd.rar",
        "RAR archive (v4)",
        ["rar", "r00", "r01", "001", "cbr"]
    ),
    l!(
        "archive.rar5",
        "archive/rar5",
        "application/vnd.rar",
        "RAR archive (v5)",
        ["rar", "r00", "r01", "001", "cbr"]
    ),
    l!(
        "archive.7z",
        "archive/7z",
        "application/x-7z-compressed",
        "7-Zip archive",
        ["7z", "001"]
    ),
    l!(
        "archive.tar",
        "archive/tar",
        "application/x-tar",
        "tar archive",
        ["tar"]
    ),
    l!(
        "archive.gzip",
        "archive/gzip",
        "application/gzip",
        "gzip compressed data",
        ["gz", "tgz"]
    ),
    l!(
        "archive.bzip2",
        "archive/bzip2",
        "application/x-bzip2",
        "bzip2 compressed data",
        ["bz2", "tbz"]
    ),
    l!(
        "archive.xz",
        "archive/xz",
        "application/x-xz",
        "xz compressed data",
        ["xz", "txz"]
    ),
    l!(
        "archive.lzma",
        "archive/lzma",
        "application/x-lzma",
        "LZMA compressed data",
        ["lzma"]
    ),
    l!(
        "archive.zstd",
        "archive/zstd",
        "application/zstd",
        "Zstandard compressed data",
        ["zst"]
    ),
    l!(
        "archive.lz4",
        "archive/lz4",
        "application/x-lz4",
        "LZ4 compressed data",
        ["lz4"]
    ),
    l!(
        "archive.lzip",
        "archive/lzip",
        "application/x-lzip",
        "lzip compressed data",
        ["lz"]
    ),
    l!(
        "archive.compress",
        "archive/compress",
        "application/x-compress",
        "Unix compress data",
        ["z"]
    ),
    l!(
        "archive.cab",
        "archive/cab",
        "application/vnd.ms-cab-compressed",
        "Microsoft Cabinet",
        ["cab"]
    ),
    l!(
        "archive.cpio",
        "archive/cpio",
        "application/x-cpio",
        "cpio archive",
        ["cpio"]
    ),
    l!(
        "archive.xar",
        "archive/xar",
        "application/x-xar",
        "xar archive",
        ["xar", "pkg"]
    ),
    l!(
        "archive.ar",
        "archive/ar",
        "application/x-archive",
        "ar archive",
        ["a", "ar", "lib"]
    ),
    l!(
        "archive.warc",
        "archive/warc",
        "application/warc",
        "WARC web archive",
        ["warc"]
    ),
    // File-list structures (`listing::classify`): folders and split files, no bytes.
    l!(
        "archive.multipart",
        "archive/multipart",
        "application/octet-stream",
        "Multi-volume archive / split file (from a file list)",
        ["001", "002", "r00", "r01", "z01"]
    ),
    l!(
        "game.pkg.split",
        "game/pkg-split",
        "application/octet-stream",
        "Split PS4 / PS5 package (from a file list)",
        ["pkg"]
    ),
    l!(
        "game.switch.nsp.split",
        "game/switch-nsp-split",
        "application/octet-stream",
        "FAT32-split Switch package (from a file list)",
        []
    ),
    l!(
        "game.ps5.folder",
        "game/ps5-folder",
        "inode/directory",
        "PS5 game folder dump",
        []
    ),
    l!(
        "game.ps4.folder",
        "game/ps4-folder",
        "inode/directory",
        "PS4 game folder dump",
        []
    ),
    l!(
        "game.vita.folder",
        "game/vita-folder",
        "inode/directory",
        "PS Vita game folder dump",
        []
    ),
    l!(
        "game.ps3.folder",
        "game/ps3-folder",
        "inode/directory",
        "PS3 game folder (JB / disc) dump",
        []
    ),
    l!(
        "game.psp.folder",
        "game/psp-folder",
        "inode/directory",
        "PSP UMD folder dump",
        []
    ),
    l!(
        "game.wiiu.folder",
        "game/wiiu-folder",
        "inode/directory",
        "Wii U game folder (loadiine / WUP) dump",
        []
    ),
    l!(
        "game.wiiu.nus",
        "game/wiiu-nus",
        "inode/directory",
        "Nintendo CDN (NUS) download: title.tmd + .app",
        []
    ),
    l!(
        "game.xbox.folder",
        "game/xbox-folder",
        "inode/directory",
        "Xbox game folder dump",
        []
    ),
    l!(
        "game.x360.folder",
        "game/x360-folder",
        "inode/directory",
        "Xbox 360 game folder dump",
        []
    ),
    l!(
        "game.x360.god",
        "game/x360-god",
        "inode/directory",
        "Xbox 360 Games on Demand (STFS header + data files)",
        []
    ),
    l!(
        "disc.bluray.folder",
        "disc/bluray-folder",
        "inode/directory",
        "Blu-ray (BDMV) folder",
        []
    ),
    l!(
        "disc.dvd_video.folder",
        "disc/dvd-video-folder",
        "inode/directory",
        "DVD-Video (VIDEO_TS) folder",
        []
    ),
    // ---------------------------------------------------------------- packages
    l!(
        "package.arch",
        "package/arch",
        "application/x-alpm-package",
        "Arch Linux package",
        ["zst"]
    ),
    l!(
        "package.deb",
        "package/deb",
        "application/vnd.debian.binary-package",
        "Debian package",
        ["deb"]
    ),
    l!(
        "package.rpm",
        "package/rpm",
        "application/x-rpm",
        "RPM package",
        ["rpm"]
    ),
    l!(
        "package.msi",
        "package/msi",
        "application/x-msi",
        "Windows Installer package",
        ["msi"]
    ),
    l!(
        "package.msp",
        "package/msp",
        "application/x-ms-patch",
        "Windows Installer patch",
        ["msp"]
    ),
    l!(
        "package.apk",
        "package/apk",
        "application/vnd.android.package-archive",
        "Android package",
        ["apk"]
    ),
    l!(
        "package.xapk",
        "package/xapk",
        "application/x-xapk",
        "Android split package bundle",
        ["xapk", "apkm"]
    ),
    l!(
        "package.aab",
        "package/aab",
        "application/x-aab",
        "Android app bundle",
        ["aab"]
    ),
    l!(
        "package.ipa",
        "package/ipa",
        "application/x-ios-app",
        "iOS app package",
        ["ipa"]
    ),
    l!(
        "package.jar",
        "package/jar",
        "application/java-archive",
        "Java archive",
        ["jar"]
    ),
    l!(
        "package.wheel",
        "package/wheel",
        "application/x-wheel+zip",
        "Python wheel",
        ["whl"]
    ),
    l!(
        "package.nupkg",
        "package/nupkg",
        "application/x-nupkg",
        "NuGet package",
        ["nupkg"]
    ),
    l!(
        "package.appx",
        "package/appx",
        "application/vnd.ms-appx",
        "Windows app package",
        ["appx", "msix"]
    ),
    l!(
        "package.macos_pkg",
        "package/macos-pkg",
        "application/x-newton-compatible-pkg",
        "macOS installer package",
        ["pkg", "mpkg"]
    ),
    // ---------------------------------------------------------------- executables
    l!(
        "exe.pe",
        "exe/pe",
        "application/vnd.microsoft.portable-executable",
        "Windows PE executable",
        ["exe", "dll", "sys", "scr", "ocx", "cpl", "efi"]
    ),
    l!(
        "exe.pe.dotnet",
        "exe/pe-dotnet",
        "application/vnd.microsoft.portable-executable",
        ".NET assembly",
        ["exe", "dll"]
    ),
    l!(
        "exe.pe.nsis",
        "exe/pe-nsis",
        "application/vnd.microsoft.portable-executable",
        "NSIS installer",
        ["exe"]
    ),
    l!(
        "exe.pe.inno",
        "exe/pe-inno",
        "application/vnd.microsoft.portable-executable",
        "Inno Setup installer",
        ["exe"]
    ),
    l!(
        "exe.pe.sfx_rar",
        "exe/pe-sfx-rar",
        "application/vnd.microsoft.portable-executable",
        "RAR self-extractor",
        ["exe"]
    ),
    l!(
        "exe.pe.sfx_7z",
        "exe/pe-sfx-7z",
        "application/vnd.microsoft.portable-executable",
        "7-Zip self-extractor",
        ["exe"]
    ),
    l!(
        "exe.pe.sfx_zip",
        "exe/pe-sfx-zip",
        "application/vnd.microsoft.portable-executable",
        "ZIP self-extractor",
        ["exe"]
    ),
    l!(
        "exe.pe.sfx_cab",
        "exe/pe-sfx-cab",
        "application/vnd.microsoft.portable-executable",
        "Cabinet self-extractor",
        ["exe"]
    ),
    l!(
        "exe.mz_dos",
        "exe/mz-dos",
        "application/x-dosexec",
        "DOS executable",
        ["exe", "com"]
    ),
    l!(
        "exe.ne",
        "exe/ne",
        "application/x-dosexec",
        "16-bit Windows executable",
        ["exe", "dll"]
    ),
    l!(
        "exe.le",
        "exe/le",
        "application/x-dosexec",
        "LE/LX executable",
        ["exe", "vxd"]
    ),
    l!(
        "exe.elf",
        "exe/elf",
        "application/x-elf",
        "ELF executable",
        ["so", "bin", "elf", "o", "run", ""]
    ),
    l!(
        "exe.macho",
        "exe/macho",
        "application/x-mach-binary",
        "Mach-O executable",
        ["dylib", "bundle", ""]
    ),
    l!(
        "exe.appimage",
        "exe/appimage",
        "application/vnd.appimage",
        "AppImage",
        ["appimage"]
    ),
    l!(
        "exe.java_class",
        "exe/java-class",
        "application/java-vm",
        "Java class file",
        ["class"]
    ),
    l!(
        "exe.lnk",
        "exe/lnk",
        "application/x-ms-shortcut",
        "Windows shortcut",
        ["lnk"]
    ),
    l!(
        "exe.wasm",
        "exe/wasm",
        "application/wasm",
        "WebAssembly module",
        ["wasm"]
    ),
    l!(
        "exe.dex",
        "exe/dex",
        "application/x-dex",
        "Android Dalvik executable",
        ["dex"]
    ),
    // ---------------------------------------------------------------- disc and disk images
    l!(
        "disc.iso9660",
        "disc/iso9660",
        "application/x-iso9660-image",
        "ISO 9660 disc image",
        ["iso", "img"]
    ),
    l!(
        "disc.udf",
        "disc/udf",
        "application/x-udf-image",
        "UDF disc image",
        ["iso", "img"]
    ),
    l!(
        "disc.dvd_video",
        "disc/dvd-video",
        "application/x-iso9660-image",
        "DVD-Video disc image",
        ["iso", "img"]
    ),
    l!(
        "disc.bluray",
        "disc/bluray",
        "application/x-iso9660-image",
        "Blu-ray disc image",
        ["iso", "img"]
    ),
    l!(
        "disc.cd_raw",
        "disc/cd-raw",
        "application/x-cd-image",
        "Raw CD image (2352-byte sectors)",
        ["bin", "img", "raw"]
    ),
    l!(
        "disc.dmg",
        "disc/dmg",
        "application/x-apple-diskimage",
        "Apple disk image",
        ["dmg"]
    ),
    l!(
        "disc.mds",
        "disc/mds",
        "application/x-mds",
        "Alcohol 120% descriptor",
        ["mds"]
    ),
    l!(
        "disc.nrg",
        "disc/nrg",
        "application/x-nrg",
        "Nero disc image",
        ["nrg"]
    ),
    l!(
        "disc.daa",
        "disc/daa",
        "application/x-daa",
        "PowerISO DAA image",
        ["daa"]
    ),
    l!(
        "disc.qcow2",
        "disc/qcow2",
        "application/x-qemu-disk",
        "QEMU qcow2 disk image",
        ["qcow2", "qcow"]
    ),
    l!(
        "disc.vhd",
        "disc/vhd",
        "application/x-vhd",
        "Virtual PC disk image",
        ["vhd"]
    ),
    l!(
        "disc.vhdx",
        "disc/vhdx",
        "application/x-vhdx",
        "Hyper-V disk image",
        ["vhdx"]
    ),
    l!(
        "disc.vmdk",
        "disc/vmdk",
        "application/x-vmdk",
        "VMware disk image",
        ["vmdk"]
    ),
    l!(
        "disc.vdi",
        "disc/vdi",
        "application/x-virtualbox-vdi",
        "VirtualBox disk image",
        ["vdi"]
    ),
    l!(
        "disc.ext",
        "disc/ext4",
        "application/x-ext-image",
        "ext2/3/4 file system image",
        ["img", "bin", "ext4"]
    ),
    l!(
        "disc.fat",
        "disc/fat",
        "application/x-fat-image",
        "FAT file system image",
        ["img", "bin"]
    ),
    l!(
        "disc.exfat",
        "disc/exfat",
        "application/x-exfat-image",
        "exFAT file system image",
        ["img", "bin"]
    ),
    l!(
        "disc.ntfs",
        "disc/ntfs",
        "application/x-ntfs-image",
        "NTFS file system image",
        ["img", "bin"]
    ),
    l!(
        "disc.squashfs",
        "disc/squashfs",
        "application/vnd.squashfs",
        "SquashFS image",
        ["squashfs", "sfs", "img"]
    ),
    // ---------------------------------------------------------------- added with the console / signature work
    l!(
        "game.ps5.param_json",
        "game/ps5-param-json",
        "application/json",
        "PS5 param.json (folder dump metadata)",
        ["json"]
    ),
    l!(
        "game.switch.xcz",
        "game/switch-xcz",
        "application/x-switch-xcz",
        "Nintendo Switch compressed game card image (XCZ)",
        ["xcz"]
    ),
    l!(
        "game.wii.wad",
        "game/wii-wad",
        "application/x-wii-wad",
        "Wii WAD package (WiiWare / Virtual Console / channel)",
        ["wad"]
    ),
    l!(
        "game.wiiu.wud",
        "game/wiiu-wud",
        "application/x-wiiu-wud",
        "Wii U disc image (WUD)",
        ["wud", "iso"]
    ),
    l!(
        "game.wiiu.wux",
        "game/wiiu-wux",
        "application/x-wiiu-wux",
        "Wii U compressed disc image (WUX)",
        ["wux"]
    ),
    l!(
        "game.wiiu.meta",
        "game/wiiu-meta",
        "application/xml",
        "Wii U meta.xml (folder dump metadata)",
        ["xml"]
    ),
    l!(
        "game.gbc.rom",
        "game/gbc-rom",
        "application/x-gameboy-color-rom",
        "Game Boy Color ROM",
        ["gbc", "gb", "cgb"]
    ),
    l!(
        "game.snes.rom",
        "game/snes-rom",
        "application/x-snes-rom",
        "Super NES ROM",
        ["sfc", "smc", "fig", "swc"]
    ),
    l!(
        "game.x360.xex",
        "game/x360-xex",
        "application/x-xbox360-executable",
        "Xbox 360 executable (XEX)",
        ["xex"]
    ),
    l!(
        "game.x360.iso",
        "game/x360-iso",
        "application/x-xbox360-image",
        "Xbox 360 disc image (XDVDFS)",
        ["iso"]
    ),
    l!(
        "game.genesis.rom",
        "game/genesis-rom",
        "application/x-genesis-rom",
        "Sega Mega Drive / Genesis ROM",
        ["md", "gen", "bin", "smd"]
    ),
    l!(
        "game.32x.rom",
        "game/32x-rom",
        "application/x-genesis-32x-rom",
        "Sega 32X ROM",
        ["32x", "bin"]
    ),
    l!(
        "game.sms.rom",
        "game/sms-rom",
        "application/x-sms-rom",
        "Sega Master System ROM",
        ["sms"]
    ),
    l!(
        "game.gg.rom",
        "game/gg-rom",
        "application/x-gamegear-rom",
        "Sega Game Gear ROM",
        ["gg"]
    ),
    l!(
        "game.segacd.iso",
        "game/segacd-iso",
        "application/x-segacd-image",
        "Sega Mega-CD / Sega CD disc image",
        ["iso", "bin", "img"]
    ),
    l!(
        "game.saturn.iso",
        "game/saturn-iso",
        "application/x-saturn-image",
        "Sega Saturn disc image",
        ["iso", "bin", "img"]
    ),
    l!(
        "game.dreamcast.iso",
        "game/dreamcast-iso",
        "application/x-dreamcast-image",
        "Sega Dreamcast disc image / track",
        ["iso", "bin", "img", "gdi"]
    ),
    l!(
        "game.lynx.rom",
        "game/lynx-rom",
        "application/x-atari-lynx-rom",
        "Atari Lynx ROM",
        ["lnx"]
    ),
    l!(
        "game.a7800.rom",
        "game/a7800-rom",
        "application/x-atari-7800-rom",
        "Atari 7800 ROM",
        ["a78"]
    ),
    l!(
        "game.ngp.rom",
        "game/ngp-rom",
        "application/x-neo-geo-pocket-rom",
        "Neo Geo Pocket (Color) ROM",
        ["ngp", "ngc"]
    ),
    l!(
        "game.fds.disk",
        "game/fds-disk",
        "application/x-fds-disk",
        "Famicom Disk System image",
        ["fds"]
    ),
    l!(
        "exe.lua",
        "exe/lua",
        "application/x-lua-bytecode",
        "Precompiled Lua chunk",
        ["luac", "out"]
    ),
    l!(
        "exe.psx",
        "exe/psx",
        "application/x-psx-executable",
        "PlayStation executable (PS-X EXE)",
        ["exe", "psx"]
    ),
    l!(
        "exe.ps3.self",
        "exe/ps3-self",
        "application/x-ps3-self",
        "PS3 signed executable (SELF / SPRX)",
        ["self", "sprx", "bin"]
    ),
    l!(
        "exe.vita.self",
        "exe/vita-self",
        "application/x-vita-self",
        "PS Vita signed executable (SELF / SUPRX / SKPRX)",
        ["bin", "self", "suprx", "skprx"]
    ),
    l!(
        "exe.ps4.self",
        "exe/ps4-self",
        "application/x-ps4-self",
        "PS4 / PS5 signed executable (SELF)",
        ["bin", "prx", "sprx", "self"]
    ),
    l!(
        "exe.switch.nro",
        "exe/switch-nro",
        "application/x-switch-nro",
        "Nintendo Switch homebrew executable (NRO)",
        ["nro"]
    ),
    l!(
        "exe.switch.nso",
        "exe/switch-nso",
        "application/x-switch-nso",
        "Nintendo Switch executable (NSO)",
        ["nso"]
    ),
    l!(
        "exe.3ds.3dsx",
        "exe/3ds-3dsx",
        "application/x-3dsx",
        "3DS homebrew executable (3DSX)",
        ["3dsx"]
    ),
    l!(
        "exe.dol",
        "exe/dol",
        "application/x-dol",
        "GameCube / Wii executable (DOL)",
        ["dol"]
    ),
    l!(
        "exe.wiiu.wuhb",
        "exe/wiiu-wuhb",
        "application/x-wuhb",
        "Wii U homebrew bundle (WUHB)",
        ["wuhb"]
    ),
    l!(
        "misc.keepass",
        "misc/keepass",
        "application/x-keepass",
        "KeePass password database",
        ["kdbx", "kdb"]
    ),
    l!(
        "misc.ds_store",
        "misc/ds-store",
        "application/octet-stream",
        "macOS Finder .DS_Store",
        ["ds_store"]
    ),
    l!(
        "misc.git_pack",
        "misc/git-pack",
        "application/x-git-pack",
        "Git pack file",
        ["pack"]
    ),
    l!(
        "model.blend",
        "model/blend",
        "application/x-blender",
        "Blender scene",
        ["blend"]
    ),
    l!(
        "model.fbx",
        "model/fbx",
        "application/octet-stream",
        "Autodesk FBX (binary)",
        ["fbx"]
    ),
    l!(
        "model.glb",
        "model/glb",
        "model/gltf-binary",
        "Binary glTF model",
        ["glb"]
    ),
    l!(
        "gamedata.unity",
        "gamedata/unity",
        "application/x-unity-bundle",
        "Unity asset bundle",
        ["unity3d", "assets", "bundle"]
    ),
    l!(
        "gamedata.godot_pck",
        "gamedata/godot-pck",
        "application/x-godot-pck",
        "Godot resource pack",
        ["pck"]
    ),
    l!(
        "gamedata.vpk",
        "gamedata/vpk",
        "application/x-valve-vpk",
        "Valve VPK archive",
        ["vpk"]
    ),
    l!(
        "gamedata.quake_pak",
        "gamedata/quake-pak",
        "application/x-quake-pak",
        "Quake-engine PAK archive",
        ["pak"]
    ),
    l!(
        "gamedata.doom_wad",
        "gamedata/doom-wad",
        "application/x-doom-wad",
        "Doom-engine WAD",
        ["wad"]
    ),
    l!(
        "gamedata.bsa",
        "gamedata/bsa",
        "application/x-bethesda-bsa",
        "Bethesda archive (BSA)",
        ["bsa"]
    ),
    l!(
        "gamedata.ba2",
        "gamedata/ba2",
        "application/x-bethesda-ba2",
        "Bethesda archive (BA2)",
        ["ba2"]
    ),
    l!(
        "gamedata.renpy_rpa",
        "gamedata/renpy-rpa",
        "application/x-renpy-rpa",
        "Ren'Py archive",
        ["rpa"]
    ),
    l!(
        "gamedata.rgss",
        "gamedata/rgss",
        "application/x-rgss-archive",
        "RPG Maker encrypted archive",
        ["rgssad", "rgss2a", "rgss3a"]
    ),
    l!(
        "archive.ace",
        "archive/ace",
        "application/x-ace-compressed",
        "ACE archive",
        ["ace"]
    ),
    l!(
        "archive.arj",
        "archive/arj",
        "application/x-arj",
        "ARJ archive",
        ["arj"]
    ),
    l!(
        "archive.lzh",
        "archive/lzh",
        "application/x-lzh-compressed",
        "LHA / LZH archive",
        ["lzh", "lha"]
    ),
    l!(
        "archive.zpaq",
        "archive/zpaq",
        "application/x-zpaq",
        "ZPAQ archive",
        ["zpaq"]
    ),
    l!(
        "archive.wim",
        "archive/wim",
        "application/x-ms-wim",
        "Windows imaging archive (WIM / ESD)",
        ["wim", "esd", "swm"]
    ),
    l!(
        "archive.lzop",
        "archive/lzop",
        "application/x-lzop",
        "lzop compressed file",
        ["lzo"]
    ),
    l!(
        "archive.stuffit",
        "archive/stuffit",
        "application/x-stuffit",
        "StuffIt archive",
        ["sit", "sitx"]
    ),
    l!(
        "archive.zoo",
        "archive/zoo",
        "application/x-zoo",
        "Zoo archive",
        ["zoo"]
    ),
    l!(
        "audio.mod",
        "audio/mod",
        "audio/x-mod",
        "ProTracker module",
        ["mod"]
    ),
    l!(
        "audio.s3m",
        "audio/s3m",
        "audio/x-s3m",
        "Scream Tracker 3 module",
        ["s3m"]
    ),
    l!(
        "audio.xm",
        "audio/xm",
        "audio/x-xm",
        "FastTracker 2 module",
        ["xm"]
    ),
    l!(
        "audio.it",
        "audio/it",
        "audio/x-it",
        "Impulse Tracker module",
        ["it"]
    ),
    l!(
        "audio.musepack",
        "audio/musepack",
        "audio/x-musepack",
        "Musepack audio",
        ["mpc", "mp+", "mpp"]
    ),
    l!(
        "audio.nsf",
        "audio/nsf",
        "audio/x-nsf",
        "NES sound file",
        ["nsf", "nsfe"]
    ),
    l!(
        "audio.spc",
        "audio/spc",
        "audio/x-spc",
        "SNES SPC700 sound file",
        ["spc"]
    ),
    l!(
        "audio.vgm",
        "audio/vgm",
        "audio/x-vgm",
        "Video Game Music log",
        ["vgm"]
    ),
    l!(
        "audio.gbs",
        "audio/gbs",
        "audio/x-gbs",
        "Game Boy sound file",
        ["gbs"]
    ),
    l!(
        "audio.psf",
        "audio/psf",
        "audio/x-psf",
        "Portable Sound Format (PSF family)",
        [
            "psf", "minipsf", "psf2", "ssf", "dsf", "usf", "gsf", "snsf", "2sf"
        ]
    ),
    l!(
        "audio.sid",
        "audio/sid",
        "audio/prs.sid",
        "Commodore 64 SID tune",
        ["sid"]
    ),
    l!(
        "image.icns",
        "image/icns",
        "image/icns",
        "Apple icon image",
        ["icns"]
    ),
    l!(
        "image.hdr",
        "image/radiance-hdr",
        "image/vnd.radiance",
        "Radiance HDR image",
        ["hdr", "pic"]
    ),
    l!(
        "image.xcf",
        "image/xcf",
        "image/x-xcf",
        "GIMP image",
        ["xcf"]
    ),
    l!(
        "image.ktx",
        "image/ktx",
        "image/ktx",
        "Khronos texture (KTX / KTX2)",
        ["ktx", "ktx2"]
    ),
    l!(
        "image.fits",
        "image/fits",
        "image/fits",
        "FITS image",
        ["fits", "fit", "fts"]
    ),
    l!(
        "disc.apfs",
        "disc/apfs",
        "application/x-apfs",
        "APFS container",
        ["img", "dmg"]
    ),
    l!(
        "disc.hfsplus",
        "disc/hfsplus",
        "application/x-hfsplus",
        "HFS+ volume",
        ["img", "dmg", "hfs"]
    ),
    l!(
        "disc.btrfs",
        "disc/btrfs",
        "application/x-btrfs",
        "Btrfs file system",
        ["img"]
    ),
    l!(
        "disc.xfs",
        "disc/xfs",
        "application/x-xfs",
        "XFS file system",
        ["img"]
    ),
    l!(
        "disc.luks",
        "disc/luks",
        "application/x-luks",
        "LUKS encrypted volume",
        ["img", "luks"]
    ),
    l!(
        "disc.chd",
        "disc/chd",
        "application/x-chd",
        "MAME compressed hunks of data (CHD)",
        ["chd"]
    ),
    l!(
        "disc.ecm",
        "disc/ecm",
        "application/x-ecm",
        "ECM-compressed CD image",
        ["ecm"]
    ),
    l!(
        "disc.isz",
        "disc/isz",
        "application/x-isz",
        "Compressed ISO (ISZ)",
        ["isz"]
    ),
    // ---------------------------------------------------------------- console games
    l!(
        "game.ps5.pkg",
        "game/ps5-pkg",
        "application/x-ps5-pkg",
        "PlayStation 5 package",
        ["pkg"]
    ),
    l!(
        "game.ps4.pkg",
        "game/ps4-pkg",
        "application/x-ps4-pkg",
        "PlayStation 4 package",
        ["pkg"]
    ),
    l!(
        "game.ps3.pkg",
        "game/ps3-pkg",
        "application/x-ps3-pkg",
        "PlayStation 3 package",
        ["pkg"]
    ),
    l!(
        "game.vita.pkg",
        "game/psvita-pkg",
        "application/x-psvita-pkg",
        "PlayStation Vita package",
        ["pkg"]
    ),
    l!(
        "game.vita.vpk",
        "game/psvita-vpk",
        "application/zip",
        "PlayStation Vita homebrew package (VPK)",
        ["vpk"]
    ),
    l!(
        "game.psp.pkg",
        "game/psp-pkg",
        "application/x-psp-pkg",
        "PSP package",
        ["pkg"]
    ),
    l!(
        "game.ps1.iso",
        "game/ps1-iso",
        "application/x-ps1-image",
        "PlayStation disc image",
        ["iso", "img", "bin"]
    ),
    l!(
        "game.ps1.bin",
        "game/ps1-bin",
        "application/x-ps1-image",
        "PlayStation raw disc image",
        ["bin", "img"]
    ),
    l!(
        "game.ps2.iso",
        "game/ps2-iso",
        "application/x-ps2-image",
        "PlayStation 2 disc image",
        ["iso", "img", "bin"]
    ),
    l!(
        "game.psp.iso",
        "game/psp-iso",
        "application/x-psp-image",
        "PSP UMD image",
        ["iso", "img"]
    ),
    l!(
        "game.ps3.iso",
        "game/ps3-iso",
        "application/x-ps3-image",
        "PlayStation 3 disc image",
        ["iso", "img"]
    ),
    l!(
        "game.psp.cso",
        "game/psp-cso",
        "application/x-psp-cso",
        "PSP compressed ISO",
        ["cso", "zso"]
    ),
    l!(
        "game.psp.pbp",
        "game/psp-pbp",
        "application/x-psp-pbp",
        "PSP EBOOT.PBP",
        ["pbp"]
    ),
    l!(
        "game.ps.param_sfo",
        "game/ps-param-sfo",
        "application/x-param-sfo",
        "PlayStation PARAM.SFO",
        ["sfo"]
    ),
    l!(
        "game.ciso",
        "game/ciso",
        "application/x-ciso",
        "CISO compressed disc image",
        ["ciso", "cso"]
    ),
    l!(
        "game.gcz",
        "game/gcz",
        "application/x-gcz",
        "Dolphin GCZ disc image",
        ["gcz"]
    ),
    l!(
        "game.gamecube.iso",
        "game/gamecube-iso",
        "application/x-gamecube-image",
        "GameCube disc image",
        ["iso", "img", "bin", "gcm"]
    ),
    l!(
        "game.wii.iso",
        "game/wii-iso",
        "application/x-wii-image",
        "Wii disc image",
        ["iso", "img", "bin"]
    ),
    l!(
        "game.wii.wbfs",
        "game/wii-wbfs",
        "application/x-wbfs",
        "Wii WBFS image",
        ["wbfs"]
    ),
    l!(
        "game.rvz",
        "game/rvz",
        "application/x-rvz",
        "Dolphin RVZ/WIA disc image",
        ["rvz", "wia"]
    ),
    l!(
        "game.switch.nsp",
        "game/switch-nsp",
        "application/x-nintendo-nsp",
        "Nintendo Switch NSP",
        ["nsp"]
    ),
    l!(
        "game.switch.nsz",
        "game/switch-nsz",
        "application/x-nintendo-nsz",
        "Nintendo Switch NSZ",
        ["nsz"]
    ),
    l!(
        "game.switch.xci",
        "game/switch-xci",
        "application/x-nintendo-xci",
        "Nintendo Switch cartridge image",
        ["xci", "xcz"]
    ),
    l!(
        "game.switch.ncz",
        "game/switch-ncz",
        "application/x-nintendo-ncz",
        "Nintendo Switch NCZ",
        ["ncz"]
    ),
    l!(
        "game.switch.hfs0",
        "game/switch-hfs0",
        "application/x-nintendo-hfs0",
        "Nintendo Switch HFS0 partition",
        ["hfs0"]
    ),
    l!(
        "game.3ds.cci",
        "game/3ds-cci",
        "application/x-3ds-cci",
        "Nintendo 3DS cartridge image",
        ["3ds", "cci"]
    ),
    l!(
        "game.3ds.cxi",
        "game/3ds-cxi",
        "application/x-3ds-cxi",
        "Nintendo 3DS executable image",
        ["cxi"]
    ),
    l!(
        "game.3ds.cia",
        "game/3ds-cia",
        "application/x-3ds-cia",
        "Nintendo 3DS installable archive",
        ["cia"]
    ),
    l!(
        "game.nes.rom",
        "game/nes-rom",
        "application/x-nes-rom",
        "NES ROM",
        ["nes"]
    ),
    l!(
        "game.gb.rom",
        "game/gb-rom",
        "application/x-gameboy-rom",
        "Game Boy ROM",
        ["gb", "gbc"]
    ),
    l!(
        "game.gba.rom",
        "game/gba-rom",
        "application/x-gba-rom",
        "Game Boy Advance ROM",
        ["gba"]
    ),
    l!(
        "game.nds.rom",
        "game/nds-rom",
        "application/x-nintendo-ds-rom",
        "Nintendo DS ROM",
        ["nds"]
    ),
    l!(
        "game.n64.rom",
        "game/n64-rom",
        "application/x-n64-rom",
        "Nintendo 64 ROM",
        ["z64", "n64", "v64"]
    ),
    l!(
        "game.xbox.xiso",
        "game/xbox-xiso",
        "application/x-xbox-iso",
        "Xbox XDVDFS disc image",
        ["iso", "xiso"]
    ),
    l!(
        "game.xbox.xbe",
        "game/xbox-xbe",
        "application/x-xbe",
        "Xbox executable",
        ["xbe"]
    ),
    l!(
        "game.x360.stfs",
        "game/x360-stfs",
        "application/x-xbox360-stfs",
        "Xbox 360 STFS package",
        ["", "stfs"]
    ),
    l!(
        "game.xbox_one.xvd",
        "game/xbox-xvd",
        "application/x-xvd",
        "Xbox One / Series XVD package",
        ["xvd", "xvc", ""]
    ),
    // ---------------------------------------------------------------- subtitles
    l!(
        "subtitle.srt",
        "subtitle/srt",
        "application/x-subrip",
        "SubRip subtitles",
        ["srt"]
    ),
    l!(
        "subtitle.webvtt",
        "subtitle/webvtt",
        "text/vtt",
        "WebVTT subtitles",
        ["vtt"]
    ),
    l!(
        "subtitle.ass",
        "subtitle/ass",
        "text/x-ass",
        "Advanced SubStation subtitles",
        ["ass"]
    ),
    l!(
        "subtitle.ssa",
        "subtitle/ssa",
        "text/x-ssa",
        "SubStation Alpha subtitles",
        ["ssa"]
    ),
    l!(
        "subtitle.microdvd",
        "subtitle/microdvd",
        "text/x-microdvd",
        "MicroDVD subtitles",
        ["sub", "txt"]
    ),
    l!(
        "subtitle.mpl2",
        "subtitle/mpl2",
        "text/x-mpl2",
        "MPL2 subtitles",
        ["txt", "mpl"]
    ),
    l!(
        "subtitle.sami",
        "subtitle/sami",
        "application/x-sami",
        "SAMI subtitles",
        ["smi", "sami"]
    ),
    l!(
        "subtitle.vobsub_idx",
        "subtitle/vobsub-idx",
        "text/x-vobsub-idx",
        "VobSub index",
        ["idx"]
    ),
    l!(
        "subtitle.vobsub",
        "subtitle/vobsub",
        "application/x-vobsub",
        "VobSub subtitles",
        ["sub"]
    ),
    l!(
        "subtitle.pgs",
        "subtitle/pgs",
        "application/x-pgs",
        "Blu-ray PGS subtitles",
        ["sup"]
    ),
    l!(
        "subtitle.ttml",
        "subtitle/ttml",
        "application/ttml+xml",
        "TTML subtitles",
        ["ttml", "dfxp", "xml"]
    ),
    // ---------------------------------------------------------------- text
    l!(
        "text.nfo",
        "text/nfo",
        "text/x-nfo",
        "NFO release notes",
        ["nfo", "diz", "txt"]
    ),
    l!(
        "text.plain",
        "text/plain",
        "text/plain",
        "Plain text",
        ["txt", "text", "log", "me", "1st", "diz"]
    ),
    l!(
        "text.url",
        "text/url",
        "application/x-mswinurl",
        "Internet shortcut",
        ["url"]
    ),
    l!(
        "text.html",
        "text/html",
        "text/html",
        "HTML document",
        ["html", "htm"]
    ),
    l!(
        "text.playlist",
        "text/playlist",
        "audio/x-mpegurl",
        "Playlist",
        ["m3u", "m3u8", "pls"]
    ),
    l!(
        "text.sfv",
        "text/sfv",
        "text/x-sfv",
        "SFV checksum list",
        ["sfv"]
    ),
    l!(
        "text.checksum",
        "text/checksum",
        "text/x-checksum",
        "Checksum list",
        ["md5", "sha1", "sha256", "sha"]
    ),
    l!(
        "text.json",
        "text/json",
        "application/json",
        "JSON",
        ["json"]
    ),
    l!("text.xml", "text/xml", "application/xml", "XML", ["xml"]),
    l!(
        "text.ini",
        "text/ini",
        "text/x-ini",
        "INI configuration",
        ["ini", "inf", "cfg"]
    ),
    l!(
        "text.shell",
        "text/shell",
        "text/x-shellscript",
        "Shell script",
        ["sh", "bash", "run", "shar"]
    ),
    l!(
        "text.python",
        "text/python",
        "text/x-python",
        "Python script",
        ["py"]
    ),
    l!(
        "text.batch",
        "text/batch",
        "application/x-bat",
        "Windows batch script",
        ["bat", "cmd"]
    ),
    l!(
        "text.powershell",
        "text/powershell",
        "text/x-powershell",
        "PowerShell script",
        ["ps1", "psm1"]
    ),
    l!(
        "text.vbscript",
        "text/vbscript",
        "text/vbscript",
        "VBScript",
        ["vbs", "vbe"]
    ),
    l!(
        "text.cue",
        "text/cue",
        "application/x-cue",
        "CUE sheet",
        ["cue"]
    ),
    l!(
        "text.ccd",
        "text/ccd",
        "text/x-clonecd",
        "CloneCD descriptor",
        ["ccd"]
    ),
    // ---------------------------------------------------------------- misc
    l!(
        "misc.torrent",
        "misc/torrent",
        "application/x-bittorrent",
        "BitTorrent metainfo",
        ["torrent"]
    ),
    l!(
        "misc.par2",
        "misc/par2",
        "application/x-par2",
        "PAR2 recovery set",
        ["par2"]
    ),
    l!(
        "misc.sqlite",
        "misc/sqlite",
        "application/vnd.sqlite3",
        "SQLite database",
        ["sqlite", "db", "sqlite3"]
    ),
    l!(
        "misc.pyc",
        "misc/pyc",
        "application/x-python-code",
        "Python bytecode",
        ["pyc"]
    ),
    l!(
        "misc.pgp",
        "misc/pgp",
        "application/pgp",
        "PGP armored data",
        ["asc", "sig", "gpg"]
    ),
    l!(
        "misc.riff",
        "misc/riff",
        "application/x-riff",
        "RIFF container (other form)",
        []
    ),
    l!(
        "misc.cfb",
        "misc/cfb",
        "application/x-ole-storage",
        "OLE compound file",
        []
    ),
    l!(
        "misc.thumbs_db",
        "misc/thumbs-db",
        "application/x-ole-storage",
        "Windows thumbnail cache",
        ["db"]
    ),
    l!(
        "misc.zeros",
        "misc/zeros",
        "application/octet-stream",
        "All-zero data (padding or preallocated)",
        ["pad", ""]
    ),
    l!(
        "misc.empty",
        "misc/empty",
        "application/x-empty",
        "Empty file",
        []
    ),
    l!(
        "misc.unknown",
        "misc/unknown",
        "application/octet-stream",
        "Unrecognised data",
        []
    ),
];

/// Registry entry for `id`.
pub fn info(id: &str) -> Option<&'static LabelInfo> {
    LABELS.iter().find(|l| l.id == id)
}

/// Registry entry for a prototype label (`video/matroska`).
pub fn from_legacy(legacy: &str) -> Option<&'static LabelInfo> {
    LABELS.iter().find(|l| l.legacy == legacy)
}

/// Usual extensions of `id` (empty for unknown labels).
pub fn extensions(id: &str) -> &'static [&'static str] {
    info(id).map_or(&[], |l| l.extensions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_legacy_are_unique() {
        let mut ids: Vec<&str> = LABELS.iter().map(|l| l.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate id");
        let mut leg: Vec<&str> = LABELS.iter().map(|l| l.legacy).collect();
        leg.sort_unstable();
        leg.dedup();
        assert_eq!(leg.len(), n, "duplicate legacy label");
        for l in LABELS {
            assert!(
                l.id.bytes().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || c == b'.'
                    || c == b'_'),
                "{}",
                l.id
            );
        }
    }
}

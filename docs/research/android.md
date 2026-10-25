# Android research: uniffi + cargo-ndk, librqbit on Android, Gradle/AGP/JDK, platform rules

Researched 2026-10-25. Everything marked **VERIFIED** was actually run on this machine or read from
primary sources: crates.io API, maven-metadata.xml, developer.android.com, docs.gradle.org, and
uniffi-rs docs at tag v0.32.2. Anything not run or read is marked **UNVERIFIED**.
Scratch spike: `/tmp/claude-1000/magnetar-research/android/` (Rust crate `rqspike` + Gradle project `app-test/`).

## 0. Machine gotchas (read first)

1. **`/usr/bin/cargo` shadows rustup.** Arch's packaged rust (`rustc 1.99.1`, `/usr/bin`) comes first
   in PATH and has **no Android std**. The result is `error[E0463]: can't find crate for core ... x86_64-linux-android`.
   The rustup toolchain `~/.cargo/bin` (stable **1.99.0**) has both `aarch64-linux-android` and
   `x86_64-linux-android`. Every Android build script must do:
   ```sh
   export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
   export ANDROID_NDK_HOME="$HOME/Android/Sdk/ndk/29.0.14206865"
   ```
   Do not put `rust-version` > 1.99.0 in Cargo.toml.
2. **The laptop btrfs root shows checksum corruption.** During the Gradle build, the kernel logged
   `BTRFS warning (device dm-0): csum failed root 257 ino 10138146 ...` and the `corrupt` counter reached 3.
   The bad file was a freshly written Gradle transform:
   `~/.gradle/caches/9.8.2/transforms/50d4efed85fa401b9e1eeefeadecb514/transformed/ui/jars/classes.jar`.
   It surfaced as `java.io.IOException: Erreur d'entrée/sortie` in `:app:mergeDebugJavaResource`.
   That file was left untouched. The workaround was a separate `GRADLE_USER_HOME`
   (`/tmp/claude-1000/magnetar-research/android/gradle-home`). A file corrupted minutes after it was
   written points at hardware (RAM, the USB enclosure, or the SSD). **Report this to the user. It is
   not a Magnetar issue.** If a build fails with EIO, check
   `journalctl -k | grep -i csum` before debugging the toolchain.
3. **The existing AVD `Medium_Phone_API_37.0` stays `unauthorized` over adb**, even after a cold boot
   (`-no-snapshot`) and an adb server restart. The keys in `~/.android/adbkey{,.pub}` match (modulus
   checked). A fresh AVD built from the same image (`system-images;android-37.0;google_apis_playstore;x86_64`)
   authorizes immediately, so the old AVD's userdata holds stale key state. Use a fresh AVD, or wipe
   that one (the user decides). When the AVD lives on tmpfs, the emulator needs about 7.4 GB free even with
   `disk.dataPartition.size=1G`.
4. `pkill -f <pattern>` inside a Bash tool call kills the tool's own shell, because the pattern is in
   its command line. Use `adb emu kill`.

## 1. Versions (VERIFIED via crates.io API / maven-metadata.xml, 2026-10-25)

| Thing | Version | Notes |
|---|---|---|
| librqbit | **9.0.1** (2026-08-20) | features below |
| uniffi / uniffi_bindgen | **0.32.2** (2026-09-23) | features: `cli`, `tokio`, `build`, `bindgen` |
| rusqlite | **0.41.0** (2026-10-24, MSRV 1.89) | `bundled` → libsqlite3-sys 0.39.1, SQLite **3.54.1** |
| cargo-ndk | 4.1.2 (`~/.local/bin`) | |
| NDK | 29.0.14206865 | AGP 9.4 default NDK is 28.2.13676358; we don't use AGP's NDK (cargo-ndk builds the .so) |
| AGP | **9.4.1** (latest stable; 9.5.0-rc01 / 9.6.0-alpha01 exist) | max API 37, **min Gradle 9.6.0**, build-tools 36.0.0, JDK 17+ |
| Gradle | **9.8.2** (cached) | officially runs on **JDK 27 since 9.8.0** (docs.gradle.org compat table) |
| Kotlin (KGP) | **2.4.21** (2.5.0-Beta2 exists) | Gradle 9.8 embeds Kotlin 2.4.10 (that only affects build scripts) |
| Compose compiler plugin | `org.jetbrains.kotlin.plugin.compose` **2.4.21** (same version as Kotlin) | |
| Compose BOM | **2026.09.00** → compose ui 1.12.1 | **requires compileSdk 37** |
| material3 | 1.4.0 stable via BOM (1.5.0-beta02 exists) | |
| activity-compose | 1.13.0 | |
| lifecycle-runtime-compose / viewmodel-compose | 2.11.0 | |
| navigation-compose | 2.10.2 | |
| core-ktx | 1.19.1 | |
| work-runtime-ktx | **2.12.0** | |
| JNA | **5.19.1** (`net.java.dev.jna:jna:5.19.1@aar`) | uniffi needs JNA ≥ 5.12.0 |
| kotlinx-coroutines-android | 1.11.0 | needed for uniffi `suspend` funs |
| kotlinx-serialization-json | 1.12.0 | if needed |

## 2. JDK 27 / Gradle / AGP: VERIFIED working, no extra JDK needed

The build below ran with **system JDK 27.0.1, Gradle 9.8.2, AGP 9.4.1, KGP 2.4.21 (built-in Kotlin),
compileSdk 37, targetSdk 36, minSdk 26**. It produced `app-debug.apk`, which installed and ran on an
API 37 x86_64 emulator. You do **not** need a Temurin 21 download.
- Do **not** use `kotlin { jvmToolchain(17) }`. It needs an installed JDK 17 or the foojay resolver plugin.
  Compile on JDK 27 targeting 17 instead:
  ```kotlin
  compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
  kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }
  ```
- Fallback if a future Gradle/AGP rejects JDK 27: put a Temurin 21 at `~/.local/share/jdk/temurin-21`
  and set `org.gradle.java.home=/home/dude/.local/share/jdk/temurin-21` in `gradle.properties`.
  (UNVERIFIED: not needed, so not tried.)
- **AGP 9 built-in Kotlin**: do NOT apply `org.jetbrains.kotlin.android`. If you do, the build fails with
  "Cannot add extension with name 'kotlin'" / "plugin is no longer required since AGP 9.0".
  AGP depends on KGP 2.2.10 by default. Applying `org.jetbrains.kotlin.plugin.compose` 2.4.21 in the
  root `plugins {}` raised the resolved KGP to **2.4.21** (VERIFIED in the dependency cache).
  `android.kotlinOptions {}` is gone; use `kotlin { compilerOptions {} }`.
- `compileSdk = 37` works with the installed `platforms/android-37.0` (source.properties `AndroidVersion.ApiLevel=37.0`).
  With `compileSdk = 36`, Compose BOM 2026.09.00 fails the AAR metadata check
  ("Dependency 'androidx.compose.ui:ui-android:1.12.1' requires ... compile against version 37 or later").
- Gradle wrapper: generate it with `gradle wrapper --gradle-version 9.8.2` using
  `~/.gradle/wrapper/dists/gradle-9.8.2-bin/8wepis63pzuhrlfaiy04bagwu/gradle-9.8.2/bin/gradle`.
  `--no-daemon` builds take 2–5 minutes cold.
- `local.properties`: `sdk.dir=/home/dude/Android/Sdk`.

Working files (VERIFIED):

`settings.gradle.kts`
```kotlin
pluginManagement { repositories { google(); mavenCentral(); gradlePluginPortal() } }
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories { google(); mavenCentral() }
}
rootProject.name = "magnetar"
include(":app")
```
`build.gradle.kts`
```kotlin
plugins {
    id("com.android.application") version "9.4.1" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
}
```
`gradle.properties`
```
org.gradle.jvmargs=-Xmx3g -Dfile.encoding=UTF-8
android.useAndroidX=true
```
`app/build.gradle.kts`
```kotlin
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}
android {
    namespace = "dev.magnetar"
    compileSdk = 37
    defaultConfig {
        applicationId = "dev.magnetar"
        minSdk = 26
        targetSdk = 36
        versionCode = 1; versionName = "0.1"
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }
    buildFeatures { compose = true }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    packaging { jniLibs { useLegacyPackaging = false } }
}
kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }
dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0")
    implementation("androidx.navigation:navigation-compose:2.10.2")
    implementation("androidx.work:work-runtime-ktx:2.12.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
}
```
targetSdk 36 rather than 37: either works with the installed platforms. Choosing 36 avoids opting into
API 37 behaviour changes nobody has reviewed (UNVERIFIED what those changes are). No Google Play
Services and no Firebase anywhere; the deps above are plain AndroidX.

## 3. uniffi Kotlin bindings with cargo-ndk (VERIFIED end to end)

### Rust side (ffi crate)
```toml
[lib]
crate-type = ["cdylib", "lib"]   # cdylib for Android .so; lib so the bindgen bin / tests can link
[dependencies]
uniffi = { version = "0.32.2", features = ["tokio", "cli"] }   # "cli" only needed for the in-crate bindgen bin
[[bin]]
name = "uniffi-bindgen"
path = "uniffi-bindgen.rs"     # fn main() { uniffi::uniffi_bindgen_main() }
```
Proc-macro mode, no UDL: `uniffi::setup_scaffolding!();` at the top of lib.rs, then
`#[derive(uniffi::Record)]`, `#[derive(uniffi::Object)]`, `#[derive(uniffi::Error)]`,
`#[uniffi::export] impl X { #[uniffi::constructor] pub fn new(..) -> Result<Arc<Self>, E> }`.
Errors need `Display`. Async export uses `#[uniffi::export(async_runtime = "tokio")]`, which becomes a
Kotlin `suspend fun` and needs kotlinx-coroutines. (Async is UNVERIFIED in the spike; the spike used
sync calls with an owned `tokio::runtime::Runtime` inside the Object and `rt.block_on`.)

`uniffi.toml` next to the crate's Cargo.toml:
```toml
[bindings.kotlin]
package_name = "dev.magnetar.ffi"
android = true            # Android optimisations; android_cleaner → android.system.SystemCleaner on API ≥ 33, JNA cleaner fallback
```

### Build the .so files
```sh
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH" ANDROID_NDK_HOME="$HOME/Android/Sdk/ndk/29.0.14206865"
cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/app/src/main/jniLibs build --release -p magnetar-ffi --lib
```
- `-o` copies to `jniLibs/<abi>/lib<name>.so` (VERIFIED: `jniLibs/arm64-v8a/librqspike.so`, `jniLibs/x86_64/librqspike.so`).
- `-P 26` sets the platform/API level of the NDK clang (matches minSdk 26).
- NEEDED libs are only `libdl.so libm.so libc.so`: **no `libc++_shared.so`** needed, even with ring,
  bundled SQLite and librqbit (VERIFIED with llvm-readelf).
- LOAD segment alignment is `0x4000` = **16 KB pages**, which satisfies the Android 15+ 16 KB requirement
  (VERIFIED; the NDK 29 default).
- Times: x86_64 cold 5m36s; arm64 cold 3m41s on this machine.

### Generate the Kotlin bindings
**Gotcha (VERIFIED):** `[profile.release] strip = true` removes the UniFFI metadata.
`generate --library <stripped .so>` then prints `No UniFFI metadata found in ...` and writes nothing.
Generate from the unstripped **host** build instead:
```sh
cargo build -p magnetar-ffi --lib          # host debug build, target/debug/libmagnetar_ffi.so
cargo run -p magnetar-ffi --bin uniffi-bindgen -- generate \
    --library target/debug/libmagnetar_ffi.so --language kotlin \
    --out-dir android/app/src/main/java   # or a generated dir added to sourceSets
# add --no-format to silence "Unable to auto-format ... ktlint" (harmless warning; ktlint not installed)
```
Alternative: keep `strip = "debuginfo"` (UNVERIFIED that metadata survives that) or generate from the
Android `.so` before stripping. The generated file is `<out>/<package path>/<crate>.kt` (about 1.6k lines for the spike).
The library name is baked in by `--library` (`findLibraryName` returns `"rqspike"`, which loads `librqspike.so`).
Uniffi's gradle.md suggests an Exec task; simpler is a `build-android.sh` that runs cargo-ndk and then
bindgen, committed or run before `./gradlew assembleDebug`.

Kotlin usage (VERIFIED running on the emulator):
```kotlin
val v = uniffi.rqspike.sqliteVersion()                       // "3.54.1"
val e = uniffi.rqspike.Engine(filesDir.resolve("dl").absolutePath, filesDir.resolve("state").absolutePath)
val id = e.addMagnet("magnet:?xt=urn:btih:...")              // ULong for u64
e.stats().forEach { s -> s.name; s.progressBytes; s.totalBytes; s.finished }
```
- `u64` maps to `ULong`, `Option<String>` to `String?`, `Vec<Record>` to `List<Record>`. Objects implement
  `AutoCloseable`; call `close()` or use `.use {}`. Lists of objects must be closed one by one (uniffi lifetimes.md).
- **Release/R8: UNVERIFIED.** Standard practice is `-keep class com.sun.jna.** { *; }`,
  `-keep class * implements com.sun.jna.** { *; }` and `-keep class dev.magnetar.ffi.** { *; }` in
  proguard-rules.pro. Either test `assembleRelease` with minify, or ship with `isMinifyEnabled = false`.
- uniffi docs: if Rust threads call back into Kotlin (callback interfaces) often, attach the threads to
  the JVM permanently (`jni` crate, `on_thread_start`). Otherwise JNA attaches and detaches on every call.
  Prefer polling or `suspend` returns over callbacks.

## 4. librqbit on Android: VERIFIED it compiles AND downloads

Cargo.toml in the spike (also the recommended ffi crate config):
```toml
librqbit = { version = "9.0.1", default-features = false, features = ["rust-tls"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
rusqlite = { version = "0.41.0", features = ["bundled"] }   # bundled build sets -DSQLITE_ENABLE_FTS5, FTS3, JSON1, RTREE (libsqlite3-sys 0.39.1 build.rs)
```
- librqbit 9.0.1 features: `default = ["default-tls","http-api-client"]`; `default-tls` = reqwest
  native-tls (OpenSSL), so **avoid it on Android**. `rust-tls` = reqwest/rustls plus sha1-ring.
  Other features: `http-api` (axum), `postgres` (sqlx session persistence), `webui`, `watch`, `prometheus`,
  `upnp-serve-adapter`, `storage_middleware`, `disable-upload`, `tracing-subscriber-utils`.
- Result: `cargo ndk -t x86_64` and `-t arm64-v8a`, release, **both build cleanly**. Stripped `.so` sizes:
  arm64 8.7 MB, x86_64 9.7 MB, with `opt-level="z"`, `lto="thin"`, `strip=true` and uniffi + rusqlite included.
- Runtime (x86_64 emulator, API 37): `Session::new_with_opts` OK. A magnet for Sintel
  (`08ada5a7a6183aae1e09d831df6748d566095a10`, with opentrackr and stealth.si UDP trackers) resolved its
  metadata and downloaded 57 MB of 129 MB within seconds. After an app restart, session persistence
  restored the torrent (`torrents=1` at startup). **arm64 was compiled but not run on a device (UNVERIFIED at runtime).**
- **Gotcha: set explicit paths.** By default librqbit puts the session JSON and DHT persistence in
  `directories::ProjectDirs` ("com.rqbit.session" / "com.rqbit.dht"). On Android there is no XDG/HOME,
  so that path is wrong or fails (UNVERIFIED which). The spike passes app-private paths:
  ```rust
  use librqbit::{Session, SessionOptions, SessionPersistenceConfig, DhtSessionConfig};
  let opts = SessionOptions {
      dht: Some(DhtSessionConfig { bootstrap_addrs: None, port: None,
          persistence: Some(librqbit::dht::DhtPersistenceConfig {   // librqbit re-exports `dht`
              dump_interval: None, config_filename: Some(state_dir.join("dht.json")) }) }),
      persistence: Some(SessionPersistenceConfig::Json { folder: Some(state_dir.join("session")) }),
      fastresume: true,
      ..Default::default()
  };
  let session = Session::new_with_opts(output_dir, opts).await?;   // returns Arc<Session>
  ```
  9.x API notes (VERIFIED from source): the field is `dht: Option<DhtSessionConfig>` (None disables DHT).
  The old `disable_dht` field no longer exists. Other fields: `disable_trackers`, `listen: Option<ListenerOptions>`,
  `ratelimits: LimitsConfig`, `peer_limit`, `disable_upload`, `disable_local_service_discovery`,
  `ipv4_only`, `client_name_and_version`, `trackers: HashSet<Url>`, `runtime_worker_threads`, `cancellation_token`.
  - `session.add_torrent(AddTorrent::from_url(magnet), Option<AddTorrentOptions>)` returns
    `AddTorrentResponse::{Added(id, handle), AlreadyManaged(id, handle), ListOnly(_)}`.
  - `session.with_torrents(|it| it.map(|(id, h)| ...))`; `h.name() -> Option<String>`;
    `h.stats() -> TorrentStats { state, file_progress, error, progress_bytes, uploaded_bytes, total_bytes, finished, live }`.
- Use the app's internal or app-specific external storage (`getExternalFilesDir(DIRECTORY_DOWNLOADS)`) as
  output_dir. Writing to the public Downloads folder needs MediaStore/SAF, which librqbit can't write
  through (UNVERIFIED alternative: download privately, then copy into MediaStore when finished).
- Workspace gotcha: `libsqlite3-sys` has `links = "sqlite3"`. If the server crate enables sqlx's
  `sqlite` feature, Cargo resolves the whole workspace together and you get a links conflict or a forced
  shared version. Keep sqlx to `postgres` only (librqbit's `postgres` feature also uses sqlx postgres only).
- **Fallback (keep it):** if the embedded engine fails on a device, `Intent(ACTION_VIEW, Uri.parse(magnet))`
  hands off to an external client (section 7).

## 5. WorkManager (crawl + sync + cleanup)
```kotlin
val constraints = Constraints.Builder()
    .setRequiredNetworkType(NetworkType.UNMETERED)   // Wi-Fi
    .setRequiresCharging(true)
    .setRequiresBatteryNotLow(true)                  // optional
    .build()
val req = PeriodicWorkRequestBuilder<CrawlWorker>(6, TimeUnit.HOURS)   // min period 15 min
    .setConstraints(constraints)
    .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 10, TimeUnit.MINUTES)
    .build()
WorkManager.getInstance(ctx).enqueueUniquePeriodicWork("crawl", ExistingPeriodicWorkPolicy.UPDATE, req)
```
`CoroutineWorker.doWork()` calls the ffi crawl/sync (as `suspend` funs or `withContext(Dispatchers.IO)`).
API names are standard WorkManager API, not re-read from 2.12.0 sources (treat as UNVERIFIED, low risk).
WorkManager runs with no Google services. Saved-search local polling can use the same periodic worker
(no network constraint, or CONNECTED). Workers stop when constraints are lost; make crawl/sync idempotent
and cursor-based.

## 6. Foreground service for downloads (VERIFIED from developer.android.com)
- Manifest:
  ```xml
  <uses-permission android:name="android.permission.INTERNET"/>
  <uses-permission android:name="android.permission.FOREGROUND_SERVICE"/>
  <uses-permission android:name="android.permission.FOREGROUND_SERVICE_DATA_SYNC"/>
  <uses-permission android:name="android.permission.POST_NOTIFICATIONS"/>
  <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/>
  <service android:name=".download.DownloadService" android:exported="false"
           android:foregroundServiceType="dataSync"/>
  ```
- Call `ServiceCompat.startForeground(this, id, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)`.
  Type `dataSync` has no runtime prerequisites.
- **Android 15+ (targetSdk ≥ 35): dataSync is capped at 6 hours per 24 hours**, shared by all of the app's
  dataSync services. At the limit the system calls `Service.onTimeout(int startId, int fgsType)`, and you
  must call `stopSelf()` within seconds. Otherwise:
  `RemoteServiceException: "A foreground service of type dataSync did not stop within its timeout"`.
  Starting another dataSync FGS after the limit throws `ForegroundServiceStartNotAllowedException`.
  The timer resets when the user brings the app to the foreground. **Implement onTimeout to pause the
  librqbit session, persist state, post a "paused, open app to resume" notification, and stopSelf.**
- Since Android 15, a dataSync FGS may not start from a BOOT_COMPLETED receiver, so don't auto-resume downloads at boot.
  Start the FGS only from user interaction (the "Download" button), which gives the full 6 hours.
- POST_NOTIFICATIONS (API 33+) is a runtime permission. Request it with
  `rememberLauncherForActivityResult(RequestPermission())`. If it is denied, FGS notices still show in
  Task Manager but not in the drawer, and saved-search alerts are silent. Handle the denied case in the UI.
- The Android docs recommend alternatives to dataSync (user-initiated data transfer jobs) for some
  cases. UNVERIFIED whether those fit a torrent engine; dataSync with onTimeout is the pragmatic choice.

## 7. Opening magnets in external clients (VERIFIED guidance, package-visibility docs)
```kotlin
try {
    startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(magnetUri)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
} catch (e: ActivityNotFoundException) { /* snackbar: "No torrent app installed" + copy magnet */ }
```
From Android 11, `resolveActivity()` returns null unless you declare `<queries>`. Recommended pattern:
call `startActivity` and catch `ActivityNotFoundException`. If the UI must know beforehand (to grey out a button), add:
```xml
<queries><intent><action android:name="android.intent.action.VIEW"/><data android:scheme="magnet"/></intent></queries>
```
Copy-to-clipboard: `ClipboardManager.setPrimaryClip(ClipData.newPlainText("magnet", uri))`.
Optionally register Magnetar itself for `magnet:` (intent-filter with VIEW + BROWSABLE + scheme magnet)
to accept magnets from browsers.

## 8. Phone → server URL / cleartext
The server runs in Termux on `http://127.0.0.1:<port>`. With targetSdk ≥ 28, cleartext HTTP is blocked
by default. Add `res/xml/network_security_config.xml` that permits cleartext for `127.0.0.1` / `localhost`
(and the Tailscale `100.64.0.0/10` hosts or MagicDNS name, or use Tailscale HTTPS), referenced by
`android:networkSecurityConfig`. (Standard platform behaviour, UNVERIFIED in the spike: the spike made
no HTTP calls to localhost.) Make the server URL a setting (DataStore or a row in the Rust SQLite DB).

## 9. Room vs SQLite
No Room. The phone cache is the Rust SQLite (rusqlite bundled, FTS5 compiled in) behind the uniffi
`Store`. Kotlin only calls ffi methods. Open the DB at `context.getDatabasePath("magnetar.db")` or under
`filesDir` and pass the path to the ffi constructor. Use one long-lived ffi object per process, guarded
by a Mutex in Rust (or rusqlite connection pool/WAL) because WorkManager workers and the UI run concurrently.
WAL mode is recommended (`PRAGMA journal_mode=WAL`).

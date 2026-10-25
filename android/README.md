# Magnetar for Android

Kotlin + Jetpack Compose (Material 3) app over the Rust `magnetar-ffi` library (uniffi,
package `app.magnetar.ffi`). No Google Play Services, no Firebase, no analytics.

## Build

```sh
scripts/build-android.sh                  # .so + bindings + debug and release APKs
SKIP_NATIVE=1 scripts/build-android.sh    # Kotlin/resources only, reuse the native outputs
GRADLE_TASKS=testDebugUnitTest SKIP_NATIVE=1 scripts/build-android.sh
```

The script:

1. builds `libmagnetar_ffi.so` for `arm64-v8a` and `x86_64` with cargo-ndk (`-P 26`) into
   `app/src/main/jniLibs/`;
2. builds the host debug library and generates the Kotlin bindings into
   `app/src/main/java/app/magnetar/ffi/` (from the unstripped host build, since stripping removes the
   uniffi metadata);
3. creates `android/keystore/` on first run and runs `./gradlew assembleDebug assembleRelease`.

`jniLibs/` and `app/magnetar/ffi/` are gitignored; the script regenerates them, so run it after any
change to `crates/ffi/src/lib.rs`.

Toolchain (ARCHITECTURE.md §8.1b): JDK 27, Gradle wrapper 9.8.2, AGP 9.4.1 with built-in Kotlin,
compose compiler plugin 2.4.21, compileSdk 37, targetSdk 36, minSdk 26. Rust comes from rustup
(`~/.cargo/bin`), not Arch's `/usr/bin/cargo`, which has no Android std.

If Gradle fails with `Erreur d'entrée/sortie` / `IOException: Input/output error`, check
`journalctl -k | grep -i csum`. This laptop's btrfs has logged checksum failures on freshly written
Gradle cache files. Point `GRADLE_USER_HOME` at a fresh directory and delete the transform it names.

## Release signing

`assembleRelease` signs with a local key at `android/keystore/release.jks`. Its passwords are in
`android/keystore/keystore.properties`. Both are generated on first build and gitignored.
This key is for side-loading only, not a Play upload key. **Back the directory up.** An APK
signed with a different key cannot update an installed copy; you would have to uninstall first,
which deletes the app's cache and settings. If `keystore.properties` is missing, release builds
fall back to the debug key. Release builds are not minified (`isMinifyEnabled = false`), because
JNA has not been tested with R8.

## Server URL, cleartext and Tailscale

The default server is the Termux server on the phone itself, `http://127.0.0.1:8787`.
`res/xml/network_security_config.xml` allows cleartext HTTP **only** to `127.0.0.1` and `localhost`.
For a server reached over Tailscale:

- preferred: serve it over HTTPS (`tailscale cert` / `tailscale serve`) and use
  `https://<host>.<tailnet>.ts.net`;
- or add the MagicDNS name or `100.x.y.z` address as another `<domain>` in the network security
  config and rebuild.

Settings warns when a plain `http://` URL points anywhere other than loopback.

## App structure

| Piece | Where |
|---|---|
| `MagnetarApp` | the single `Magnetar` instance, opened lazily off the main thread at `getDatabasePath("magnetar.db")`; settings flow |
| Settings | `SharedPreferences` → `FfiConfig` (`Settings.kt`); downloads in `getExternalFilesDir(DOWNLOADS)/magnetar`, librqbit state in `filesDir/rqbit` |
| Screens | `ui/` (Search, Detail, Saved, Downloads, Status, Settings) with one ViewModel each |
| `CrawlSyncWorker` | hourly unique periodic work (unmetered + charging + battery not low): crawl 120 s → sync → cleanup → poll saved searches → notifications. "Crawl now" / "Sync now" enqueue a one-time run that only needs a network connection |
| `DownloadService` | `dataSync` foreground service, started only from a user tap; progress notification; `onTimeout` (Android 15+ 6 h cap) pauses every download and stops |
| Links | the app handles `magnet:` links (opens the detail page for that infohash) and `magnetar://torrent/<infohash>` (used by notifications) |

ffi errors (`FfiException`, Rust panics as `InternalException`, a missing native library) become
snackbars or inline messages (`Errors.kt`). Unit tests (JVM, no device): `./gradlew testDebugUnitTest`.

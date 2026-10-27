# Magnetar for Android

Kotlin + Jetpack Compose (Material 3) client of a Magnetar server's REST API: search, saved
searches with notifications, Stats and Settings. No Rust inside, no downloads (magnets go to
your torrent app), no phone crawler. No Google Play Services, no Firebase, no analytics.
Design, screen → endpoint table and stages: **docs/APP.md** (ARCHITECTURE.md D79–D84).

## Build

```sh
scripts/build-android.sh                                     # unit tests + debug and release APKs
GRADLE_TASKS="assembleDebug testDebugUnitTest" scripts/build-android.sh
cd android && ./gradlew testDebugUnitTest                    # JVM unit tests only (TestNG)
```

Plain Gradle: the script creates `android/keystore/` on first run (see below) and runs
`./gradlew`. Needs the JDK and the Android SDK only (no Rust, cargo-ndk or NDK since D79).

Toolchain (ARCHITECTURE.md §8.1): JDK 27, Gradle wrapper 9.8.2, AGP 9.4.1 with built-in Kotlin,
compose and serialization compiler plugins 2.4.21, compileSdk 37, targetSdk 36, minSdk 26.
`versionName` is `[workspace.package] version` from the root `Cargo.toml`.

If Gradle fails with `Erreur d'entrée/sortie` / `IOException: Input/output error`, check
`journalctl -k | grep -i csum`. This laptop's btrfs has logged checksum failures on freshly written
Gradle cache files. Point `GRADLE_USER_HOME` at a fresh directory and delete the transform it names.

## Release signing

`assembleRelease` signs with a local key at `android/keystore/release.jks`. Its passwords are in
`android/keystore/keystore.properties`. Both are generated on first build and gitignored.
This key is for side-loading only, not a Play upload key. **Back the directory up.** An APK
signed with a different key cannot update an installed copy; you would have to uninstall first,
which deletes the app's settings. If `keystore.properties` is missing, release builds fall back
to the debug key (and `scripts/build-android.sh` generates a new key: run it only in the checkout
that holds the real one). Release builds are not minified yet (docs/APP.md §10).

The rebuilt app keeps the package name `app.magnetar` and the key, so it installs over the old
app; the server URL, token, trust root, threshold and switches carry over (docs/APP.md §7).

## Server URL, cleartext and Tailscale

There is no default server: the app asks for one. The server runs elsewhere (docs/DEPLOY.md),
typically behind `tailscale serve`, so the usual URL is `https://<host>.<tailnet>.ts.net`.

`res/xml/network_security_config.xml` permits cleartext, because the host is only known at
runtime and a network security config can only name hosts at build time. Settings warns when
an `http://` URL points at a host where the traffic would be readable: anything but loopback
and Tailscale (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`, `*.ts.net`). See ARCHITECTURE.md D39.
(The warning returns with the Settings stage; the skeleton has no URL field yet.)

## App structure

| Piece | Where |
|---|---|
| `MagnetarApp` | settings store (migration runs first), shared `OkHttpClient`, the `MagnetarApi` for the current server; removes the old app's background work |
| `api/` | `MagnetarApi` (one suspend function per endpoint, bearer token only to the server's origin, call timeout per request class, cancellable), `Models.kt` (kotlinx.serialization mirrors of the server types), `ApiException` |
| `settings/` | `AppSettings`, `SettingsStore` (SharedPreferences `settings`, the old app's keys, version 1) |
| `ui/` | `AppShell` (bottom bar: Search, Saved, Stats, Settings), `theme/` (Material You + the D47 chart colours), one package per screen |
| `Notifications` | channel `saved_searches` (the old id) |
| Links | `magnetar://torrent/<infohash>` (used by notifications) |

Unit tests (JVM, no device): `./gradlew testDebugUnitTest` — `MagnetarApiTest` (MockWebServer)
and `SettingsStoreTest` (migration from the old app's preferences).

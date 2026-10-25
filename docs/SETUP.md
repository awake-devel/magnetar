# Building and running from source

## Laptop toolchain (verified in docs/research/android.md §0 and rust-crates.md §0)

- **PATH gotcha:** Arch's `/usr/bin/cargo` and `rustc` come first in PATH and have **no
  Android std**, which fails with `E0463 can't find crate for core`. Every Android build needs:
  ```sh
  export PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH
  export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865
  ```
  The scripts set both.
- rustup stable with the targets `aarch64-linux-android` and `x86_64-linux-android`. The
  workspace MSRV is 1.94 (sqlx 0.9).
- `cargo-ndk` 4.x (here 4.1.2 in `~/.local/bin`) and NDK r29. NDK r29 produces the 16 KB
  aligned LOAD segments that Android 15+ requires.
- JDK 17 or newer (system JDK 27 works with Gradle 9.8.2), AGP 9.4.1, compileSdk 37. Kotlin
  bindings are generated from the unstripped host build.
- No OpenSSL and no aws-lc in our code: TLS is rustls with ring and webpki-roots. librqbit
  pulls `aws-lc-rs` for downloads, and that still cross-compiles with cargo-ndk.
- If a Gradle build dies with `Erreur d'entrée/sortie` / EIO, check
  `journalctl -k | grep -i csum`. This laptop has logged btrfs checksum errors in `~/.gradle`.
  Use a separate `GRADLE_USER_HOME` to route around it, and look at the hardware.

## Scripts

| Script | What |
|---|---|
| `scripts/build-termux.sh [--target arm64-v8a\|x86_64] [--out DIR]` | `cargo ndk -t arm64-v8a -t x86_64 --platform 24 build --release -p magnetar-server` → `dist/magnetar-server-{aarch64,x86_64}-linux-android`; prints size, sha256, interpreter, `NEEDED` (must be only `libc.so libm.so libdl.so`) |
| `scripts/build-android.sh` | `.so` (cargo-ndk `-P 26`) + Kotlin bindings + APK (owned by the Android build) |
| `scripts/test.sh [--ignored] [--termux] [--no-android]` | the "done" gate (see README) |
| `scripts/termux-docker-test.sh [--mode services\|nohup\|both] [--binary PATH] [--keep]` | runs `termux-setup.sh` inside `termux/termux-docker:x86_64` |
| `scripts/release.sh [--version V] [--suffix S] [--prerelease] [--notes FILE] [--no-apk] [--build\|--publish]` | dry run by default; `--build` fills `dist/` + `SHA256SUMS`; only `--publish` runs `gh release create` |
| `scripts/termux-setup.sh`, `scripts/termux-ctl.sh` | run **inside Termux**, see docs/TERMUX.md |

`CARGO_BUILD_JOBS` defaults to 3 in the scripts. Set it higher when nothing else is building.

## Release assets

`scripts/release.sh --publish` uploads everything in `dist/`:
`magnetar-server-aarch64-linux-android`, `magnetar-server-x86_64-linux-android`,
`magnetar-<tag>.apk`, `termux-setup.sh`, `termux-ctl.sh` and `SHA256SUMS`. The Termux setup
fetches the binary, `SHA256SUMS` and the two scripts by these exact names. Publishing
refuses to run with a dirty tree, an unpushed commit or an existing tag. A debug-signed APK
is shipped (with a warning) when no release APK was built. **UNVERIFIED:** the APK path
`android/app/build/outputs/apk/**` depends on the Android build, which is not on this
branch yet.

## Running the server on a Linux machine

1. Install Postgres 18 with contrib (`pg_trgm`, `unaccent`). Create the role and database,
   then the extensions:
   ```sql
   CREATE ROLE magnetar LOGIN PASSWORD '...';
   CREATE DATABASE magnetar OWNER magnetar;
   \c magnetar
   CREATE EXTENSION IF NOT EXISTS pg_trgm;
   CREATE EXTENSION IF NOT EXISTS unaccent;
   ```
2. Run `cargo build --release -p magnetar-server`.
3. Create an env file with `DATABASE_URL`, `MAGNETAR_API_TOKEN` (`openssl rand -hex 32`),
   `MAGNETAR_BIND=127.0.0.1:8787`, and optionally `MAGNETAR_ROOT_NPUB` and ntfy (see
   docs/CONFIG.md).
4. Run `magnetar-server migrate`, then `magnetar-server selftest`, then
   `magnetar-server serve`, under whatever supervisor you prefer.
5. Expose it to the phone with `tailscale serve --bg 127.0.0.1:8787` (HTTPS on
   `https://<machine>.<tailnet>.ts.net`; MagicDNS and HTTPS certificates must be enabled for
   the tailnet). See the README's Tailscale section.

## Tests and Postgres

Tests that need Postgres read `MAGNETAR_TEST_DATABASE_URL`. Locally that is
`postgres://magnetar:magnetar@127.0.0.1:54329/magnetar`, from the docker container
`magnetar-pg` running `postgres:18` (18.6, the same major as Termux). Each test creates and
drops its own schema `t_<random>`. If the variable is unset, the DB tests print
`SKIPPED (no MAGNETAR_TEST_DATABASE_URL)` and pass. `scripts/test.sh` always sets it, so a
green `test.sh` means they actually ran. Network tests against real relays and trackers
are `#[ignore]`: run them with `scripts/test.sh --ignored`.

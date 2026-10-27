# Building and running from source

## Laptop toolchain (verified in docs/research/android.md §0 and rust-crates.md §0)

- rustup stable (or Arch's cargo) for the server; the workspace MSRV is 1.94 (sqlx 0.9).
- The Android app is plain Kotlin since D79 (docs/APP.md): JDK 17 or newer (system JDK 27 works
  with Gradle 9.8.2), AGP 9.4.1, the Android SDK with compileSdk 37. No Rust Android targets,
  cargo-ndk or NDK any more.
- No OpenSSL and no aws-lc in our code: TLS is rustls with ring and webpki-roots (librqbit
  pulls `aws-lc-rs` for the server's downloads).
- If a Gradle build dies with `Erreur d'entrée/sortie` / EIO, check
  `journalctl -k | grep -i csum`. This laptop has logged btrfs checksum errors in `~/.gradle`.
  Use a separate `GRADLE_USER_HOME` to route around it, and look at the hardware.

## Scripts

| Script | What |
|---|---|
| `scripts/build-linux.sh <x86_64\|aarch64>` | static server binary `dist/magnetar-server-<arch>-linux` for that architecture, on a matching host (docs/LINUX.md). CI's release workflow calls it |
| `scripts/build-android.sh` | Gradle: unit tests + debug and release APKs (android/README.md) |
| `scripts/test.sh [--ignored] [--no-android]` | the "done" gate (see README) |
| `scripts/release.sh [--version V] [--suffix S] [--prerelease] [--notes FILE] [--no-apk] [--build\|--publish]` | finishes a release CI started: dry run by default; `--build` builds the APK into `dist/`; `--publish` uploads it and publishes |
| `docker build .` / `docker compose up -d --build` | the server image from source (docs/DOCKER.md) |

`CARGO_BUILD_JOBS` defaults to 3 in the scripts. Set it higher when nothing else is building.

## Continuous integration (`.github/workflows/`, ARCHITECTURE.md D39)

| Workflow | When | What |
|---|---|---|
| `ci.yml` | push to `main`, pull requests | `cargo fmt --check`, clippy `-D warnings`, `cargo test --workspace --all-features` against a `postgres:18` service (`MAGNETAR_TEST_DATABASE_URL` set, so the DB tests run), and an amd64 image build without push. Network tests stay `#[ignore]` |
| `docker.yml` | push to `main`, tags `v*` | image per architecture on native runners (`ubuntu-24.04`, `ubuntu-24.04-arm`), pushed by digest, merged into one manifest list `ghcr.io/awake-devel/magnetar`: `X.Y.Z` and `latest` for a release tag, only `X.Y.Z-rc1` for a prerelease tag, `edge` for `main` |
| `release.yml` | tags `v*` (or run by hand with a tag) | checks tag = Cargo version, `scripts/build-linux.sh` on both runners, `--version` smoke run, `SHA256SUMS`, attached to the release (created as a draft if missing) |

The Android app is not built in CI: the signing key stays on the laptop.

## Making a release

1. Bump `version` in `Cargo.toml` (`[workspace.package]`), add `docs/release-notes/v<version>.md`,
   commit and push `main`.
2. `git tag v<version> && git push origin v<version>` (a suffix like `v0.2.0-rc1` makes a
   prerelease). CI builds the image and the Linux binaries and creates the draft release.
3. Wait for the `release` workflow (`gh run list --workflow release.yml`), check out the tag,
   then `scripts/release.sh` (plan), `scripts/release.sh --publish` (add `--suffix rc1` for a
   prerelease tag). It refuses to run with a dirty tree, with HEAD not at the pushed tag, or
   before CI attached both binaries.
4. Once, after the first `docker.yml` run (the first push to `main` with it): that run
   creates the package `ghcr.io/awake-devel/magnetar`, and GitHub makes a new package on a
   personal account private. Make it public (github.com/awake-devel > Packages > magnetar > Package settings > Change
   visibility > Public), or `docker compose pull` fails with `denied` for everyone not logged in.
   The `merge` job of `docker.yml` warns while an anonymous pull is still refused.

Release assets: `magnetar-server-x86_64-linux`, `magnetar-server-aarch64-linux` (CI),
`magnetar-<tag>.apk`, `compose.yml`, `.env.example`, `tailscale-serve.json` (from `docker/`;
release.sh) and one `SHA256SUMS` that both sides merge into. A debug-signed APK is shipped
(with a warning) when no release APK was built.

## Running the server

See [docs/DEPLOY.md](DEPLOY.md): Docker Compose ([docs/DOCKER.md](DOCKER.md)) or the native
binary with your own Postgres 18 ([docs/LINUX.md](LINUX.md)). For a quick local run from the
checkout: create the role, database and the `pg_trgm` and `unaccent` extensions, set
`DATABASE_URL` and `MAGNETAR_API_TOKEN`, then `cargo run --release -p magnetar-server -- selftest`
and `-- serve`.

## Tests and Postgres

Tests that need Postgres read `MAGNETAR_TEST_DATABASE_URL`. Locally that is
`postgres://magnetar:magnetar@127.0.0.1:54329/magnetar`, from the docker container
`magnetar-pg` running `postgres:18` (the image `compose.yml` uses too). Each test creates and
drops its own schema `t_<random>`. If the variable is unset, the DB tests print
`SKIPPED (no MAGNETAR_TEST_DATABASE_URL)` and pass. `scripts/test.sh` always sets it, so a
green `test.sh` means they actually ran. Network tests against real relays and trackers
are `#[ignore]`: run them with `scripts/test.sh --ignored`.

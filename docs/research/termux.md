# Research: running the Magnetar server in Termux

Researched 2026-10-25. **[V]** means I checked it against a primary source or ran it here. **UNVERIFIED** means I did not.
Scratch experiments are in `/tmp/claude-1000/magnetar-research/termux/`.

## 0. TL;DR decisions

| Topic | Decision |
|---|---|
| Postgres | Termux `postgresql` **18.6** [V]. `pg_trgm`, `unaccent` and `french.stop` ship in the main package [V], so no contrib package is needed. |
| Data dir | `$PREFIX/var/lib/postgresql`. This is the only path the stock runit service actually uses (bug: see 1.4) [V]. |
| Listen | `listen_addresses = '127.0.0.1'`, plus the unix socket at `$PREFIX/tmp` [V]. Server connects via the **socket**. |
| Auth | `local all all trust` and `host all all 127.0.0.1/32 scram-sha-256`. Other Android apps can reach loopback, so a password is needed on TCP. |
| Binary | **Cross-compile on the laptop**: `cargo ndk -t arm64-v8a --platform 24 build --release -p magnetar-server`. Output is a PIE linked against bionic `libc/libm/libdl` only [V]. On-device `pkg install rust` (1.99.1) is the fallback. |
| TLS | rustls + **ring** + **webpki-roots**, passed in explicitly. No OpenSSL, no aws-lc-rs (it needs cmake/NASM in cross builds). |
| Supervision | `termux-services` (runit) 0.13: `sv-enable postgres`, plus a custom `$PREFIX/var/service/magnetar`. Termux:Boot runs `termux-wake-lock` and starts services. A `nohup` script is the fallback. |
| Killer | On Android 12+ the phantom-process killer must be disabled (Dev options toggle on 14+, or `adb shell settings put global settings_enable_monitor_phantom_procs false`). |
| ntfy | `POST https://ntfy.sh/<topic>` with headers `Title`, `Priority` (1–5), `Tags`, `Click`. Body max 4096 B. ntfy.sh allows 250 msgs/day. |

## 1. Termux PostgreSQL

### 1.1 Package facts [V]
- From `termux-packages/packages/postgresql/build.sh` (master): `TERMUX_PKG_VERSION="18.6"`. Depends: `libandroid-execinfo, libandroid-shmem, libicu, libpq, libuuid, libxml2, openssl, readline, zlib`. Configure flags: `--with-icu --with-libxml --with-openssl --with-uuid=e2fs USE_UNNAMED_POSIX_SEMAPHORES=1`.
- The aarch64 apt index (`packages.termux.dev/apt/termux-main/dists/stable/main/binary-aarch64/Packages`) lists `postgresql 18.6` and `libpq 18.6`.
- **Contrib is built into the main package.** `termux_step_post_make_install` installs `btree_gin btree_gist citext dblink fuzzystrmatch hstore pageinspect pg_freespacemap pg_stat_statements pg_trgm pgcrypto pgrowlocks postgres_fdw tablefunc unaccent uuid-ossp`. I downloaded and unpacked `postgresql_18.6_aarch64.deb`, and it contains `usr/lib/postgresql/pg_trgm.so`, `usr/share/postgresql/extension/pg_trgm.control` (up to 1.6), `unaccent.so` and `.control`, and `tsearch_data/french.stop` plus `unaccent.rules`. The old `postgresql-contrib` package is `BREAKS/REPLACES`, so **do not** try to install it.
- Termux patches that matter:
  - `DEFAULT_PGSOCKET_DIR` = `@TERMUX_PREFIX@/tmp`, which is `/data/data/com.termux/files/usr/tmp`. The `postgresql.conf.sample` comment still says `/tmp`, but that is only a comment. The compiled default is `$PREFIX/tmp`.
  - The `initdb` patch defaults `encoding = "UTF-8"` and `locale = "en_US.UTF-8"`, and `choose_dsm_implementation()` returns `"mmap"` on Android. You therefore do **not** need to set `dynamic_shared_memory_type`. SysV semaphores and shm are disabled on Android, which is why `USE_UNNAMED_POSIX_SEMAPHORES` and the `shm_open` → `$PREFIX/tmp` shim exist.
  - The package is "not safe for on-device builds". That is irrelevant to us because we install it with `pkg`.
- To match the Termux major in CI and tests, use docker `postgres:18`. It reports `PostgreSQL 18.6` [V].

### 1.2 Install and init
```sh
pkg install -y postgresql            # pulls libpq, psql, initdb, pg_ctl, contrib
PGDATA=$PREFIX/var/lib/postgresql
[ -f "$PGDATA/PG_VERSION" ] || initdb -D "$PGDATA" -U "$(whoami)" -E UTF8 --locale=C.UTF-8 -A trust
```
- `--locale=C.UTF-8` keeps collation deterministic and ICU-free. FTS does not depend on lc_collate. We use the `simple` and/or `english` configs explicitly. Whether `C.UTF-8` is accepted by bionic is **UNVERIFIED**. The fallback is to omit `--locale`, in which case the patched default `en_US.UTF-8` applies.
- The superuser is the Termux uid name (`u0_aNNN`) unless `-U` is given. The script passes `-U "$(whoami)"`, and the app role is created separately (1.3).

### 1.3 Config (idempotent edits)
Append to `$PGDATA/postgresql.conf`. The last occurrence of a GUC wins, so appending once guarded by a marker comment is idempotent.
```
# >>> magnetar
listen_addresses = '127.0.0.1'
port = 5432
max_connections = 20            # each backend = 1 process (phantom-process budget, see 3.3)
shared_buffers = 64MB
io_workers = 1                  # [V] in Termux 18.6 conf.sample: io_method=worker, io_workers=3 (range 1-32)
# <<< magnetar
```
Replace `$PGDATA/pg_hba.conf` entirely:
```
local   all  all                 trust          # $PREFIX/tmp is Termux-private (app data dir, SELinux)
host    all  all  127.0.0.1/32   scram-sha-256  # loopback is shared with ALL apps on the phone
```
Then create the role, database and extensions:
```sh
pg_ctl -D "$PGDATA" -l "$PREFIX/var/log/postgres.log" -w start   # if not already running (pg_ctl status)
psql -d postgres -tAc "select 1 from pg_roles where rolname='magnetar'" | grep -q 1 \
  || psql -d postgres -c "create role magnetar login password '$MAGNETAR_DB_PASSWORD'"
psql -d postgres -tAc "select 1 from pg_database where datname='magnetar'" | grep -q 1 \
  || createdb -O magnetar magnetar
psql -d magnetar -c "create extension if not exists pg_trgm; create extension if not exists unaccent;"
```
- **Security gotcha:** on Android, `127.0.0.1` is reachable by every installed app, because all apps share the network namespace. Never use `trust` on `host` lines. The same applies to the Magnetar HTTP API: bind `127.0.0.1` **and** require a bearer token on mutating endpoints (sync POST, downloads, pin/block).

### 1.4 Stock runit service [V], with a bug
The deb ships `$PREFIX/var/service/postgres/{run,down,log/run}`. `down` exists, so the service is disabled by default. Its `run` script is:
```sh
mkdir -p ~/.postgres
if [ -f "~/.postgres/postgresql.conf" ]; then DATADIR="~/.postgres"; else DATADIR="/data/data/com.termux/files/usr/var/lib/postgresql"; fi
exec postgres -D $DATADIR 2>&1
```
`"~/.postgres/..."` is quoted, so the tilde never expands and the test is always false. The service **always** uses `$PREFIX/var/lib/postgresql`. **Initdb exactly there**, or write our own service. Because `pg_ctl` and runit must not both manage the cluster, the setup script uses `pg_ctl` only for the one-time bootstrap, then `pg_ctl stop` and `sv-enable postgres`.

### 1.5 sqlx connection strings [V, sqlx 0.9.0 against PG 18.6 in docker]
Both of these unix-socket forms work:
- `postgres://magnetar@localhost/magnetar?host=/data/data/com.termux/files/usr/tmp`
- `postgres:///magnetar?host=/data/data/com.termux/files/usr/tmp&user=magnetar`

TCP is the plain `postgres://magnetar:<pw>@127.0.0.1:5432/magnetar`. **Default `DATABASE_URL` in Termux = socket form** (no password and no TCP exposure needed for the server). For `sqlx` compile-time macros (`query!`), build with `SQLX_OFFLINE=true` and a committed `.sqlx/` (`cargo sqlx prepare`), because the cross-build host has no DB. Alternatively, use only runtime `sqlx::query()`/`query_as()`.

## 2. Building and shipping the Rust server binary

### 2.1 Facts
- Termux packages are built at **`TERMUX_PKG_API_LEVEL=24`** (`scripts/build/termux_step_setup_variables.sh`) [V], with NDK `TERMUX_NDK_VERSION_NUM=30` in `scripts/properties.sh` [V]. The Termux app has `minSdkVersion=21` and **`targetSdkVersion=28`** [V]. targetSdk 28 is what lets Termux exec binaries in its home dir: the W^X restriction for API 29+ targets does not apply to it.
- Termux `rust` package: **1.99.1**, which depends on `clang, lld, libc++, rust-std-aarch64-linux-android` [V]. `pkg install rust` therefore gives a working native toolchain (host triple `aarch64-linux-android`). Use it as the fallback path. Expect slow builds (tens of minutes) and high RAM use on a phone, and use `CARGO_BUILD_JOBS=2` (UNVERIFIED timing).
- **Local machine gotcha [V]:** `/usr/bin/rustc` is Arch's `rust` 1.99.1 with **only** `x86_64-unknown-linux-gnu`. The rustup toolchain in `~/.cargo/bin` (`rustc 1.99.0`) has `aarch64-linux-android` and `x86_64-linux-android`. `/usr/bin` comes first in PATH, so **Android builds must run with `PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH`**, or cargo-ndk fails with a missing target. The `rustup` command itself is only at `~/.cargo/bin/rustup`.

### 2.2 Cross-compile recipe [V]
Experiment `xbuild/` uses tokio 1.54, axum 0.8.9, sqlx 0.9.0 (`postgres, runtime-tokio, tls-rustls-ring-webpki, macros, migrate`), reqwest 0.13.5 (`rustls-no-provider, json`), rustls 0.23.46 (`ring, std, tls12`) and webpki-roots 1.0.11:
```sh
export PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH
export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865
cargo ndk -t arm64-v8a -t x86_64 --platform 24 build --release      # cargo-ndk 4.1.2
```
Result (1m48s cold): `ELF 64-bit LSB pie executable, ARM aarch64 ... interpreter /system/bin/linker64, for Android 24, built by NDK r29`, about 5.4 MB stripped. `readelf -d` NEEDED shows only **`libdl.so libm.so libc.so`**. There is no libc++ and no OpenSSL, so nothing from Termux's `$PREFIX/lib` is needed. ring's C/asm compiled fine with the NDK clang.
- **Static vs dynamic:** keep the default (dynamic against system bionic, PIE). Android requires PIE. Bionic static executables are possible but discouraged and unnecessary, since the only deps are system libs. If any crate pulls in C++ (`cc` with cpp), you would need `libc++_shared.so` (Termux has `libc++`) or `-C link-arg=-static-libstdc++`. Avoid that: keep the dep tree free of C++.
- **API 24 binary in Termux:** this matches Termux's own package API level, so it is expected to run on any Android 7+ phone. **I could not run it on a device or emulator** (the AVD booted, but adb stayed `unauthorized`). Runtime execution under Termux is therefore **UNVERIFIED here**, although it is the same ABI and linker as every Termux package. First thing to check on the phone: `./magnetar-server --version`.
- DNS: the Rust std/tokio `lookup_host` calls bionic `getaddrinfo`, which goes through Android's `netd` (`/dev/socket/dnsproxyd`). It works for NDK binaries inside an app uid (UNVERIFIED on device here; this is the standard Termux behaviour). There is **no `/etc/resolv.conf` dependence**, so do not use hickory/trust-dns with system config. Use the default tokio resolver.
- `std::env::temp_dir()`: the Termux shell sets `TMPDIR=$PREFIX/tmp` (`TermuxShellEnvironment.java` puts `ENV_TMPDIR` = `TERMUX_TMP_PREFIX_DIR_PATH`) [V]. If it is unset, Rust on Android falls back to `/data/local/tmp`, which is not writable by apps. Run services with TMPDIR exported (the runit run script sources the env, see 3.2).

### 2.3 TLS details and a gotcha
- **reqwest 0.13**: both `rustls` and `rustls-no-provider` features hard-depend on **`rustls-platform-verifier`** [V, crates.io features]. On Android, platform-verifier normally needs JNI init (`rustls_platform_verifier::android::init_*`), which a Termux CLI does not have. On the Linux host the default client worked (200). On Android **without JNI it is expected to fail or panic (UNVERIFIED)**. **Rule: always build reqwest with `.tls_backend_preconfigured(rustls::ClientConfig)` using webpki-roots** [V compiles and works on host]:
  ```rust
  let _ = rustls::crypto::ring::default_provider().install_default();
  let mut roots = rustls::RootCertStore::empty();
  roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
  let cfg = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
  let http = reqwest::Client::builder().tls_backend_preconfigured(cfg).build()?;
  ```
- For WebSocket relays (nostr-sdk 0.45.4 / tokio-tungstenite), select the rustls + webpki-roots features. Avoid `rustls-native-certs` 0.8.5 on Termux: it looks for OpenSSL-style cert paths and Android keeps CAs in `/system/etc/security/cacerts` (hashed PEM). Termux's `ca-certificates` provides `$PREFIX/etc/tls/cert.pem`, but webpki-roots removes the dependency entirely. Whether native-certs finds the Termux bundle is **UNVERIFIED**.
- Use the **ring** provider, not aws-lc-rs (rustls default): aws-lc-rs 1.18 needs cmake/clang tricks for NDK cross builds. sqlx feature `tls-rustls-ring-webpki` (= `tls-rustls`) [V]. Postgres TLS is irrelevant anyway because the server uses the unix socket.

### 2.4 Getting the binary onto the phone
In order of preference:
1. **SSH over Tailscale or LAN:** in Termux, `pkg install openssh; passwd; sshd` (port **8022**). From the laptop: `scp -P 8022 target/aarch64-linux-android/release/magnetar-server <phone>:~/magnetar/bin/`.
2. `adb push … /sdcard/Download/`, then in Termux (after `termux-setup-storage`) `install -m755 ~/storage/downloads/magnetar-server ~/magnetar/bin/`. Shared storage is mounted noexec, so **copy, never exec in place**. Termux cannot read `/data/local/tmp`.
3. Fallback with no laptop: `pkg install rust git && cargo build --release -p magnetar-server` on device. The setup script supports `MAGNETAR_BUILD=local`.

## 3. Process supervision, wake lock and phantom-process killer

### 3.1 termux-services [V, README and scripts]
- `pkg install termux-services` (0.13-1, depends `runit 2.3.1`). **Restart the Termux session** afterwards so that `$PREFIX/etc/profile.d/start-services.sh` starts `service-daemon` (it exports `SVDIR=$PREFIX/var/service`, `LOGDIR=$PREFIX/var/log`).
- Commands: `sv-enable <svc>` removes `$SVDIR/<svc>/down` and runs `sv up`. `sv-disable <svc>` and `sv up|down|status <svc>` work as usual. Logs go to `$PREFIX/var/log/sv/<svc>/current` (svlogger).
- Inside a non-interactive script right after install, `SVDIR` may be unset. Export it yourself and start the daemon with `(service-daemon start >/dev/null 2>&1 &)`, which is what `start-services.sh` does.

### 3.2 Our service dir
`$PREFIX/var/service/magnetar/run`:
```sh
#!/data/data/com.termux/files/usr/bin/sh
export TMPDIR=/data/data/com.termux/files/usr/tmp
. "$HOME/.config/magnetar/env"          # DATABASE_URL, MAGNETAR_* (chmod 600)
# wait for postgres socket
while [ ! -S "$TMPDIR/.s.PGSQL.5432" ]; do sleep 1; done
exec "$HOME/magnetar/bin/magnetar-server" serve 2>&1
```
`$PREFIX/var/service/magnetar/log/run`:
```sh
#!/data/data/com.termux/files/usr/bin/sh
exec svlogger "$0"    # same pattern as packaged services (UNVERIFIED that svlogger takes this arg form — copy postgres/log/run verbatim instead)
```
Safest option: `cp $PREFIX/var/service/postgres/log/run $PREFIX/var/service/magnetar/log/run`.

### 3.3 Wake lock, boot and phantom killer
- `termux-wake-lock` / `termux-wake-unlock` (termux-tools) holds a partial wakelock through the Termux foreground notification. Also exempt Termux from battery optimisation (Android prompts on first wake-lock; see dontkillmyapp.com).
- **Termux:Boot** (F-Droid `com.termux.boot`, which must come from the **same source/signature** as Termux) [V]: launch it once, then create `~/.termux/boot/start-magnetar`:
  ```sh
  #!/data/data/com.termux/files/usr/bin/sh
  termux-wake-lock
  . /data/data/com.termux/files/usr/etc/profile.d/start-services.sh
  ```
- **Phantom-process killer (Android 12+)** [V, termux-app README and agnostic-apollo docs]: Android kills child processes of apps beyond **32 phantom processes total (all apps combined)** and kills processes that use excessive CPU, showing `[Process completed (signal 9)]`. Fixes:
  - **Android 14+:** Settings → System → Developer options → **"Disable child process restrictions"**. Developer options must stay enabled, or the setting reverts.
  - **Android 12L/13+ (also works on 14+, and persists):** `adb shell "settings put global settings_enable_monitor_phantom_procs false"`. Without a PC, use `pkg install android-tools` plus wireless adb. Check the value with `settings get global settings_enable_monitor_phantom_procs`.
  - Root: `su -c "settings put global settings_enable_monitor_phantom_procs false"`.
  - Plain Android 12 (not 12L) needs the older `device_config` commands. See the linked doc (UNVERIFIED which commands apply to the user's phone).
- **Process budget** (counts toward the 32): postmaster + checkpointer + bgwriter + walwriter + autovacuum launcher + logical-replication launcher + io_workers (3 by default on PG18) + 1 per connection; runsvdir + 1 runsv + 1 svlogd per service; magnetar-server (1 process, tokio threads do not count); the shell. That is about 20 with `sqlx max_connections=5`. **Keep the sqlx pool ≤ 5 and `io_workers=1`**. The setup script prints a warning when `settings get` is not `false` (that command needs adb, so the script just reminds the user).
- **Fallback without runit** (`magnetar-termux.sh start|stop|status`):
  ```sh
  pg_ctl -D "$PGDATA" -l "$PREFIX/var/log/postgres.log" -w start
  nohup "$HOME/magnetar/bin/magnetar-server" serve >>"$PREFIX/var/log/magnetar.log" 2>&1 &
  echo $! > "$PREFIX/var/run/magnetar.pid"
  ```

## 4. ntfy notifications [V, ntfy docs/publish.md main]
- Publish: `POST`/`PUT https://ntfy.sh/<topic>` (or a self-hosted base URL). The body is the message (≤ **4096 bytes**; anything larger becomes an attachment).
- Headers. Names are case-insensitive, and query params must be lowercase:
  - `X-Title` (`Title`, `t`). Max 1 KB.
  - `X-Priority` (`Priority`, `prio`, `p`). Values `1|min 2|low 3|default 4|high 5|max|urgent`.
  - `X-Tags` (`Tags`, `ta`). Comma-separated, max 512 B total. Emoji shortcodes map automatically.
  - `X-Click` (`Click`). Any URI. Non-http schemes open the handling app, so use `magnet:?xt=urn:btih:<hash>` or our own deep link `magnetar://torrent/<infohash>`. The deep-link behaviour in ntfy-android is **UNVERIFIED**; the docs only say "the responsible app may open".
  - `X-Markdown: yes`, `X-Actions` (e.g. `view, Open, magnetar://…`), `X-Icon`, `X-Cache: no`.
  - `X-Firebase: no` stops ntfy.sh from forwarding to FCM. This fits "no Google services", and the F-Droid ntfy app (`io.heckel.ntfy`) does not use Firebase anyway. It uses its own instant-delivery websocket with a foreground notification.
  - Auth: `Authorization: Bearer tk_…` or Basic.
- UTF-8 in headers is allowed, but RFC 2047 (`=?UTF-8?B?…?=`) is safer for titles. Torrent titles are often non-ASCII, so encode titles or put the title in the JSON form.
- **JSON form**: POST to the **root URL** (`https://ntfy.sh/`, not the topic). Body is `{"topic","message","title","tags":[..],"priority":4,"click","actions":[{"action":"view","label":"..","url":".."}],"markdown":true}`. **Prefer JSON** because it avoids the header-encoding issue.
- Limits on ntfy.sh: **250 messages/day**; request bucket of 60 that refills 1 every 5 s. 429 means rate-limited, and repeated abuse leads to an IP ban. **Batch**: one notification per saved search per poll with "N new results", never one per torrent.
- Topic names are public secrets. Generate something like `magnetar-<random 16+ chars>` and store it in config.
- Rust: `http.post(format!("{base}/")).json(&serde_json::json!({...})).send()` and treat any non-2xx as an error.

## 5. Android-app ↔ Termux-server notes (for the app agent)
- The app reaching `http://127.0.0.1:<port>` needs cleartext allowed: `res/xml/network_security_config.xml` with `<domain-config cleartextTrafficPermitted="true"><domain includeSubdomains="false">127.0.0.1</domain><domain>localhost</domain></domain-config>`. Later, the Tailscale URL is either a MagicDNS name, which also has to be listed or served over HTTPS via `tailscale cert`, or a `100.x` IP.
- Newer Android "local network access" permission rules (Android 16+) may affect LAN or Tailscale addresses. Loopback is expected to be exempt (**UNVERIFIED**; check on API 37).
- The server URL and API token live in app settings; the default is `http://127.0.0.1:8787`. Port 8787 is a proposal, not taken from any spec.

## 6. Idempotent setup script outline: `scripts/termux-setup.sh`
```sh
#!/data/data/com.termux/files/usr/bin/bash
set -euo pipefail
: "${PREFIX:?run inside Termux}"
MAG_HOME=${MAG_HOME:-$HOME/magnetar}; CFG=$HOME/.config/magnetar; PGDATA=$PREFIX/var/lib/postgresql
export TMPDIR=$PREFIX/tmp SVDIR=$PREFIX/var/service LOGDIR=$PREFIX/var/log

step() { printf '\n==> %s\n' "$*"; }

step "1 packages"                     # pkg is idempotent
pkg update -y && pkg install -y postgresql termux-services termux-tools openssl-tool curl jq
[ "${MAGNETAR_BUILD:-prebuilt}" = local ] && pkg install -y rust git

step "2 config + secrets"              # only generate if missing
mkdir -p "$CFG" "$MAG_HOME/bin" "$MAG_HOME/data"; chmod 700 "$CFG"
if [ ! -f "$CFG/env" ]; then
  DBPW=$(openssl rand -hex 24); TOKEN=$(openssl rand -hex 32)
  cat > "$CFG/env" <<EOF
DATABASE_URL=postgres:///magnetar?host=$PREFIX/tmp&user=magnetar
MAGNETAR_DB_PASSWORD=$DBPW
MAGNETAR_BIND=127.0.0.1:8787
MAGNETAR_API_TOKEN=$TOKEN
MAGNETAR_ROOT_NPUB=${MAGNETAR_ROOT_NPUB:-}
MAGNETAR_NTFY_URL=https://ntfy.sh
MAGNETAR_NTFY_TOPIC=magnetar-$(openssl rand -hex 8)
MAGNETAR_DOWNLOAD_DIR=$HOME/storage/downloads/magnetar
EOF
  chmod 600 "$CFG/env"
fi
set -a; . "$CFG/env"; set +a

step "3 initdb (once)"
[ -f "$PGDATA/PG_VERSION" ] || initdb -D "$PGDATA" -U "$(whoami)" -E UTF8 -A trust
grep -q '>>> magnetar' "$PGDATA/postgresql.conf" || cat >> "$PGDATA/postgresql.conf" <<'EOF'
# >>> magnetar
listen_addresses = '127.0.0.1'
max_connections = 20
shared_buffers = 64MB
io_workers = 1
# <<< magnetar
EOF
printf 'local all all trust\nhost all all 127.0.0.1/32 scram-sha-256\n' > "$PGDATA/pg_hba.conf"

step "4 bootstrap db"                   # temp start via pg_ctl unless runit already runs it
started_here=0
pg_ctl -D "$PGDATA" status >/dev/null 2>&1 || { pg_ctl -D "$PGDATA" -l "$LOGDIR/postgres-bootstrap.log" -w start; started_here=1; }
psql -d postgres -v ON_ERROR_STOP=1 -tAc "select 1 from pg_roles where rolname='magnetar'" | grep -q 1 \
  || psql -d postgres -v ON_ERROR_STOP=1 -c "create role magnetar login password '$MAGNETAR_DB_PASSWORD'"
psql -d postgres -tAc "select 1 from pg_database where datname='magnetar'" | grep -q 1 || createdb -O magnetar magnetar
psql -d magnetar -v ON_ERROR_STOP=1 -c 'create extension if not exists pg_trgm; create extension if not exists unaccent;'
pg_ctl -D "$PGDATA" reload >/dev/null || true
[ $started_here = 1 ] && pg_ctl -D "$PGDATA" -w stop

step "5 binary"
case "${MAGNETAR_BUILD:-prebuilt}" in
  local) (cd "$MAG_HOME/src" && CARGO_BUILD_JOBS=2 cargo build --release -p magnetar-server) \
           && install -m755 "$MAG_HOME/src/target/release/magnetar-server" "$MAG_HOME/bin/";;
  *) [ -x "$MAG_HOME/bin/magnetar-server" ] || { echo "copy aarch64 binary to $MAG_HOME/bin (scp -P 8022 …)"; exit 1; };;
esac
"$MAG_HOME/bin/magnetar-server" --version
"$MAG_HOME/bin/magnetar-server" migrate          # sqlx migrations, idempotent

step "6 services"
mkdir -p "$SVDIR/magnetar/log"
cat > "$SVDIR/magnetar/run" <<EOF   # (body from §3.2)
EOF
cp -f "$SVDIR/postgres/log/run" "$SVDIR/magnetar/log/run"; chmod +x "$SVDIR/magnetar/run" "$SVDIR/magnetar/log/run"
pgrep -f service-daemon >/dev/null || (service-daemon start >/dev/null 2>&1 &)
sleep 2; sv-enable postgres; sv-enable magnetar

step "7 boot + wakelock"
mkdir -p ~/.termux/boot
printf '#!%s/bin/sh\ntermux-wake-lock\n. %s/etc/profile.d/start-services.sh\n' "$PREFIX" "$PREFIX" > ~/.termux/boot/start-magnetar
chmod +x ~/.termux/boot/start-magnetar; termux-wake-lock

step "8 health"
for i in $(seq 30); do curl -fsS "http://127.0.0.1:8787/health" && break; sleep 1; done
sv status postgres magnetar
echo "API token: $MAGNETAR_API_TOKEN  (paste into app settings)"
echo "ntfy topic: $MAGNETAR_NTFY_TOPIC  (subscribe in ntfy F-Droid app)"
echo "REMINDER Android 12+: adb shell settings put global settings_enable_monitor_phantom_procs false"
```
Idempotency points: `pkg install` is a no-op when packages are present; secrets are generated only if `env` is missing; initdb is guarded by `PG_VERSION`; the conf append is guarded by a marker; `pg_hba` is rewritten deterministically; role and DB are checked before creation; extensions use `if not exists`; migrations are sqlx-tracked; service files are overwritten deterministically; `sv-enable` is idempotent.

Also ship `scripts/termux-ctl.sh {start|stop|status|logs}`, which wraps `sv` and has the nohup fallback from 3.3.

## 7. Open / UNVERIFIED items to check on the real phone
1. The cargo-ndk API-24 binary runs inside Termux, and DNS plus TLS work (`magnetar-server selftest`). The emulator test was blocked by adb `unauthorized`.
2. `initdb --locale=C.UTF-8` on bionic, or fall back to the default.
3. (resolved) `io_workers` exists in the Termux 18.6 conf sample, default 3, range 1-32.
4. ntfy-android handling of `magnetar://` and `magnet:` click URIs.
5. reqwest's default (platform-verifier) client fails on Android without JNI. This is assumed; the preconfigured config avoids it regardless.
6. Android 16+/API 37 local-network permission effect on loopback.

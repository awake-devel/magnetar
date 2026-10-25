# Magnetar server in Termux

This page covers the details behind the README quick start. The facts below come from
`docs/research/termux.md` unless they are marked as verified here.

## What `scripts/termux-setup.sh` does

Each step can run again safely. Re-running the script never regenerates secrets.

| Step | What | How it stays idempotent |
|---|---|---|
| 1 | `pkg install postgresql curl jq` (+ `termux-services`; + `rust git cmake` for `MAGNETAR_BUILD=local`) | only installs packages `dpkg -s` reports missing |
| 2 | `~/.config/magnetar/env` (mode 600, directory 700): `DATABASE_URL` (socket form), DB password, API token, ntfy topic, bind `127.0.0.1:8787`, download dir | each key is appended only if absent; never rewritten |
| 3 | `initdb -D $PREFIX/var/lib/postgresql -E UTF8 --locale=C.UTF-8 -A trust` | only if `PG_VERSION` is missing |
| 3 | managed block in `postgresql.conf`: `listen_addresses='127.0.0.1'`, `port=5432`, socket dir `$PREFIX/tmp`, `max_connections=20`, `shared_buffers=64MB`, `io_workers=1`. `pg_hba.conf`: `local trust`, `host 127.0.0.1/32 scram-sha-256` | block between markers is replaced; Postgres restarts only if a file changed |
| 4 | role `magnetar` (login, password from env), database `magnetar`, extensions `pg_trgm`, `unaccent` | `\gexec` with `NOT EXISTS`, `CREATE EXTENSION IF NOT EXISTS` |
| 5 | server binary into `~/magnetar/bin/magnetar-server` | kept unless `--update`; new binary must run `--version` before it replaces the old one |
| 6 | `magnetar-server migrate` | sqlx migrations are tracked |
| 7 | `magnetar-ctl` (+ `$PREFIX/bin/magnetar-ctl` symlink) and a copy of the setup script for `magnetar-ctl update` | overwritten |
| 8 | runit: stock `postgres` service enabled, `magnetar` service written (`log/run` copied from postgres) | file compared before rewrite; restart only on change |
| 9 | `~/.termux/boot/start-magnetar` (wake lock + start services), `termux-wake-lock` | overwritten |
| - | waits up to 30 s for `GET /health`, prints the token and next steps | |

The stock postgres runit service always uses `$PREFIX/var/lib/postgresql`: a quoting bug in
its `run` script means the `~/.postgres` branch never applies. That is why the cluster lives
there. `pg_ctl` is used only for the bootstrap and is stopped before runit takes over, so
the two never manage the cluster at the same time.

### Flags and environment

```
bash termux-setup.sh                      # runit services (default)
bash termux-setup.sh --no-services        # pg_ctl + nohup; boot script runs `magnetar-ctl start`
bash termux-setup.sh --update             # re-fetch/rebuild the binary, migrate, restart
bash termux-setup.sh --uninstall-services # remove the magnetar service + boot script, disable postgres; keep data
bash termux-setup.sh --print-token
```

| Variable | Effect |
|---|---|
| `MAGNETAR_BINARY=/path` | install this file instead of downloading |
| `MAGNETAR_BUILD=local` | `cargo build --release --locked -p magnetar-server` in `MAGNETAR_SRC` (default `~/magnetar/src`, cloned if missing) |
| `MAGNETAR_VERSION=v0.1.0-rc1` | release tag (default `latest`, which skips prereleases) |
| `GITHUB_TOKEN` | needed while the repo is private: the API asset endpoint is used instead of the public URL |
| `MAGNETAR_REPO` | `owner/name`, default `awake-devel/magnetar` |
| `MAGNETAR_HOME` | default `~/magnetar` |
| `MAGNETAR_ROOT_NPUB` | written to the env file on first run |
| `MAGNETAR_SKIP_HEALTH=1` | do not wait for `/health` |

Downloaded assets are `magnetar-server-aarch64-linux-android` or
`magnetar-server-x86_64-linux-android`, picked by `uname -m`. When the release has a
`SHA256SUMS` asset, the binary is checked against it and a mismatch aborts the install. The
token is passed to curl through a header file, so it never appears in the process list. curl
drops the `Authorization` header on the redirect to GitHub's storage host.

## magnetar-ctl

```
magnetar-ctl start | stop | restart | status | logs [-f] | token | health | update
```

The command uses runit when `$PREFIX/var/service/magnetar/run` exists, and pg_ctl + nohup
otherwise. With runit, `stop` lasts until the next boot. To remove autostart, use
`--uninstall-services`. Logs are in `$PREFIX/var/log/sv/{magnetar,postgres}/current`
(runit) or `$PREFIX/var/log/{magnetar,postgres}.log` (nohup). Edit
`~/.config/magnetar/env`, then run `magnetar-ctl restart`.

## Getting a binary onto the phone without a release

1. Over SSH: in Termux run `pkg install openssh && passwd && sshd` (port 8022). From the laptop:
   `scp -P 8022 dist/magnetar-server-aarch64-linux-android <phone-ip>:`.
2. With adb: `adb push dist/magnetar-server-aarch64-linux-android /sdcard/Download/`. Then, in
   Termux after `termux-setup-storage`, copy the file into `$HOME`. Shared storage is noexec,
   so never run it from there.
3. Then run `MAGNETAR_BINARY=~/magnetar-server-aarch64-linux-android bash termux-setup.sh`.

`MAGNETAR_BUILD=local` builds on the phone with Termux's `rust` package (1.99.1, which is
above the workspace MSRV of 1.94). Expect tens of minutes and a lot of RAM. **UNVERIFIED:**
the librqbit dependency pulls `aws-lc-rs`, and whether `aws-lc-sys` builds natively in Termux
(with `cmake` installed) has not been tested.

## Process budget and the phantom process killer

Android 12+ kills app child processes beyond **32 across all apps**, and also kills any that
use too much CPU. Termux then shows `[Process completed (signal 9)]`. Magnetar's footprint is
about 20 processes:

- the Postgres postmaster plus its checkpointer, bgwriter, walwriter, autovacuum launcher,
  logical replication launcher and 1 io worker
- one backend per connection (sqlx pool of 5 or fewer)
- runsvdir, plus one runsv and one svlogd per service
- the server itself (tokio threads do not count)
- your shell

To disable the killer:
- Android 14+: Developer options > **Disable child process restrictions**. Keep Developer
  options on.
- Android 12L/13+: `adb shell "settings put global settings_enable_monitor_phantom_procs false"`.
  This persists. Check it with `settings get global settings_enable_monitor_phantom_procs`.
  Without a PC, `pkg install android-tools` gives you wireless adb.
- Plain Android 12 needs older `device_config` commands (UNVERIFIED which ones apply).

The script cannot check this from inside Termux, since the setting needs the shell uid, so it
only prints a reminder.

## Boot

Termux:Boot (F-Droid `com.termux.boot`, same signature as Termux) runs
`~/.termux/boot/start-magnetar` at boot after you have opened it once. In services mode the
boot script takes the wake lock and sources `$PREFIX/etc/profile.d/start-services.sh`. In
nohup mode it runs `magnetar-ctl start`.

## Verification done so far

`scripts/termux-docker-test.sh` runs the real `termux-setup.sh` inside
`termux/termux-docker:x86_64` (bionic, real Termux packages, Postgres **18.6**). It uses a
fake server binary (`scripts/termux-docker-test/fake-magnetar-server`) that implements
`--version`, `migrate` (connects with `DATABASE_URL` and checks the extensions) and `serve`
(socat: `/health` returns 200, the API returns 401 without the token). Docker has no
`/dev/ashmem` (removed from mainline Linux in 5.18), so Termux's `libandroid-shmem` cannot
create the System V interlock segment Postgres needs. The harness therefore preloads a memfd
shim built with the NDK (`scripts/termux-docker-test/fakeshm.c`). Phones do not need it,
because Termux targets SDK 28 and still has ashmem.

Checked green in both runit and nohup mode (2026-10-25, 62 checks):
- package install from scratch
- initdb with `--locale=C.UTF-8`, which bionic accepts: the cluster collation is `C.UTF-8`
- socket at `$PREFIX/tmp/.s.PGSQL.5432`
- `DATABASE_URL` socket form logs in as `magnetar@magnetar`
- `pg_trgm` 1.6 and `unaccent` 1.1 installed, and `similarity()` works
- `listen_addresses`, `max_connections` and `io_workers` applied
- TCP with the scram password accepted, TCP without a password rejected
- migrate ran
- `/health` 200 and API 401 without the token
- runit postgres and magnetar both `run:`, svlogd log present
- second run: env file byte-identical, token unchanged, no package reinstall, no second
  initdb, one config block, still healthy
- `magnetar-ctl` stop, start, restart and update
- `--uninstall-services` keeps data

`scripts/build-termux.sh` produced both binaries on 2026-10-25. They need only
`libc.so libdl.so` and request the interpreter `/system/bin/linker64`. The x86_64 binary ran
(`--version`) inside termux-docker. That build was the contracts-only `main.rs`, so its size
(about 450 KB) says nothing about the real server.

These were not testable in docker: `termux-wake-lock` (no Android `am`), Termux:Boot, the
phantom process killer, the GitHub release download (the repo is private and has no
release yet), and the real `magnetar-server`. To run the same suite against the real
binary, use
`scripts/termux-docker-test.sh --binary dist/magnetar-server-x86_64-linux-android`.

## Troubleshooting

- `pkg install` 404: a broken mirror. Run `termux-change-repo` and pick another mirror.
- `/health` does not answer: run `magnetar-ctl logs`. Check that `MAGNETAR_API_TOKEN` is set
  and that the postgres socket exists.
- `[Process completed (signal 9)]`: the phantom process killer (see above).
- Postgres does not start after a crash: look at `$PREFIX/var/log/sv/postgres/current`. A
  stale `postmaster.pid` is handled by Postgres itself. Do not run `pg_ctl` while runit
  manages the cluster.
- Starting over (deletes the index): `bash termux-setup.sh --uninstall-services`, then
  `rm -rf ~/magnetar ~/.config/magnetar $PREFIX/var/lib/postgresql`.

#!/usr/bin/env bash
# Build release artifacts and (only with --publish) create a GitHub release with gh.
#
#   scripts/release.sh [--version X.Y.Z] [--suffix rc1] [--prerelease] [--notes FILE]
#                      [--no-apk] [--build | --publish]
#
# Default is a dry run: print the plan, build nothing, call nothing.
# --build     build into dist/ and write SHA256SUMS, but do not call gh
# --publish   build, then `gh release create v<version>[-<suffix>] dist/*`
#
# Tag: v<version> (default: workspace version in Cargo.toml) plus -<suffix> if given.
# Notes: --notes FILE, else docs/release-notes/<tag>.md if present, else the git log
# since the previous tag. Prereleases are not "latest": install them in Termux with
# MAGNETAR_VERSION=<tag> (see docs/TERMUX.md).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dist=$root/dist
version=""
suffix=""
prerelease=0
notes=""
apk=1
mode=dry-run
while [ $# -gt 0 ]; do
  case "$1" in
    --version) version=$2; shift ;;
    --suffix) suffix=$2; shift ;;
    --prerelease) prerelease=1 ;;
    --notes) notes=$2; shift ;;
    --no-apk) apk=0 ;;
    --build) mode=build ;;
    --publish) mode=publish ;;
    -h | --help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

cd "$root"
if [ -z "$version" ]; then
  version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
fi
[ -n "$version" ] || die "cannot read the workspace version from Cargo.toml"
tag=v$version${suffix:+-$suffix}
repo=$(sed -n 's#^repository *= *"https://github.com/\(.*\)"#\1#p' Cargo.toml)
repo=${repo:-awake-devel/magnetar}
commit=$(git rev-parse HEAD)

if [ -z "$notes" ] && [ -f "docs/release-notes/$tag.md" ]; then notes=docs/release-notes/$tag.md; fi
if [ -n "$notes" ]; then
  [ -f "$notes" ] || die "notes file $notes not found"
  notes_desc=$notes
else
  prev=$(git describe --tags --abbrev=0 2>/dev/null || true)
  notes_desc="generated from git log ${prev:-(all history)}..HEAD"
fi

gh_cmd=(gh release create "$tag" --repo "$repo" --target "$commit" --title "Magnetar $tag")
[ "$prerelease" = 1 ] && gh_cmd+=(--prerelease)

say "Release plan ($mode)"
say "  repo        $repo"
say "  tag         $tag  (commit ${commit:0:12}$([ "$prerelease" = 1 ] && echo ', prerelease'))"
say "  notes       $notes_desc"
say "  1. scripts/build-termux.sh --out dist   (magnetar-server-{aarch64,x86_64}-linux-android)"
if [ "$apk" = 1 ]; then
  say "  2. scripts/build-android.sh, copy the APK to dist/magnetar-$tag.apk"
else
  say "  2. (no APK: --no-apk)"
fi
say "  3. copy scripts/termux-setup.sh and scripts/termux-ctl.sh to dist/ (fetched by the setup)"
say "  4. write dist/SHA256SUMS"
say "  5. ${gh_cmd[*]} --notes-file <notes> dist/*"

if [ "$mode" = dry-run ]; then
  say ""
  say "Dry run: nothing built or published. Use --build to build, --publish to release."
  exit 0
fi

if [ "$mode" = publish ]; then
  command -v gh >/dev/null || die "gh not installed"
  [ -z "$(git status --porcelain)" ] || die "working tree is dirty: commit first"
  gh release view "$tag" --repo "$repo" >/dev/null 2>&1 && die "release $tag already exists"
  git merge-base --is-ancestor "$commit" "@{upstream}" 2>/dev/null ||
    die "commit ${commit:0:12} is not pushed: gh would tag a commit GitHub does not have"
fi

rm -rf "$dist"
mkdir -p "$dist"
bash scripts/build-termux.sh --out "$dist"

if [ "$apk" = 1 ]; then
  [ -f scripts/build-android.sh ] || die "scripts/build-android.sh missing (use --no-apk)"
  bash scripts/build-android.sh
  # Prefer a release build; a debug-signed APK still installs for personal use.
  built=$(find android/app/build/outputs/apk -name '*.apk' -path '*release*' 2>/dev/null | head -n1)
  if [ -z "$built" ]; then
    built=$(find android/app/build/outputs/apk -name '*.apk' 2>/dev/null | head -n1)
    [ -n "$built" ] && say "warning: no release APK, shipping $built"
  fi
  [ -n "$built" ] || die "no APK under android/app/build/outputs/apk"
  cp "$built" "$dist/magnetar-$tag.apk"
fi

install -m 755 scripts/termux-setup.sh scripts/termux-ctl.sh "$dist/"
sums=$(cd "$dist" && find . -maxdepth 1 -type f ! -name SHA256SUMS -printf '%f\n' | sort | xargs sha256sum)
printf '%s\n' "$sums" >"$dist/SHA256SUMS"
say ""
say "dist/:"
(cd "$dist" && ls -l)
say ""
cat "$dist/SHA256SUMS"

if [ "$mode" = build ]; then
  say ""
  say "Built. Publish with: scripts/release.sh --publish$([ "$prerelease" = 1 ] && echo ' --prerelease')${suffix:+ --suffix $suffix}"
  exit 0
fi

notes_file=$notes
if [ -z "$notes_file" ]; then
  notes_file=$(mktemp)
  trap 'rm -f "$notes_file"' EXIT
  {
    echo "## Changes"
    git log --pretty='- %s' ${prev:+"$prev..HEAD"}
    echo
    echo "Termux install: see docs/TERMUX.md. Verify downloads with SHA256SUMS."
  } >"$notes_file"
fi
"${gh_cmd[@]}" --notes-file "$notes_file" "$dist"/*

#!/usr/bin/env bash
# Finish a GitHub release: the parts CI cannot do (ARCHITECTURE.md D39).
#
#   scripts/release.sh [--version X.Y.Z] [--suffix rc1] [--prerelease] [--notes FILE]
#                      [--no-apk] [--build | --publish]
#
# CI does the server side when the tag vX.Y.Z[-suffix] is pushed: .github/workflows/release.yml
# attaches magnetar-server-{x86_64,aarch64}-linux and SHA256SUMS to a draft release, and
# docker.yml pushes ghcr.io/awake-devel/magnetar. This script builds and signs the APK here
# (the key never leaves android/keystore/), uploads it with compose.yml and .env.example,
# merges their hashes into SHA256SUMS, sets the notes and publishes the draft.
#
# Default is a dry run: print the plan, build nothing, call nothing.
# --build     build dist/magnetar-<tag>.apk and dist/SHA256SUMS.local, but do not call gh
# --publish   build, upload, publish. Needs a clean tree with HEAD at the pushed tag, and the
#             release with both server binaries (wait for the release workflow first)
#
# Tag: v<version> (default: workspace version in Cargo.toml) plus -<suffix> if given. A suffix
# implies --prerelease: prereleases never become "latest" (nor the image's latest tag).
# Notes: --notes FILE, else docs/release-notes/<tag>.md if present, else the git log since the
# previous tag.
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
    -h | --help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
[ -n "$suffix" ] && prerelease=1
tag=v$version${suffix:+-$suffix}
repo=$(sed -n 's#^repository *= *"https://github.com/\(.*\)"#\1#p' Cargo.toml)
repo=${repo:-awake-devel/magnetar}
commit=$(git rev-parse HEAD)
server_assets=(magnetar-server-x86_64-linux magnetar-server-aarch64-linux)
extra_files=()
for f in compose.yml .env.example; do [ -f "$f" ] && extra_files+=("$f"); done
prev=""

if [ -z "$notes" ] && [ -f "docs/release-notes/$tag.md" ]; then notes=docs/release-notes/$tag.md; fi
if [ -n "$notes" ]; then
  [ -f "$notes" ] || die "notes file $notes not found"
  notes_desc=$notes
else
  prev=$(git describe --tags --abbrev=0 "$commit^" 2>/dev/null || true)
  notes_desc="generated from git log ${prev:-(all history)}..HEAD"
fi
planned=()
[ "$apk" = 1 ] && planned+=("magnetar-$tag.apk")
planned+=("${extra_files[@]}" "SHA256SUMS (merged with CI's)")

say "Release plan ($mode)"
say "  repo        $repo"
say "  tag         $tag  (commit ${commit:0:12}$([ "$prerelease" = 1 ] && echo ', prerelease'))"
say "  notes       $notes_desc"
say "  0. CI, on git push origin $tag: ${server_assets[*]} + SHA256SUMS on a draft release,"
say "     image ghcr.io/$repo:${tag#v}"
if [ "$apk" = 1 ]; then
  say "  1. scripts/build-android.sh, copy the release APK to dist/magnetar-$tag.apk"
else
  say "  1. (no APK: --no-apk)"
fi
say "  2. gh release upload $tag --clobber ${planned[*]}"
say "  3. gh release edit $tag --notes-file <notes> --draft=false --prerelease=$([ "$prerelease" = 1 ] && echo true || echo false)"

if [ "$mode" = dry-run ]; then
  say ""
  say "Dry run: nothing built or published. Use --build to build, --publish to release."
  exit 0
fi

if [ "$mode" = publish ]; then
  command -v gh >/dev/null || die "gh not installed"
  [ -z "$(git status --porcelain)" ] || die "working tree is dirty: commit first"
  tag_commit=$(git rev-parse -q --verify "refs/tags/$tag^{commit}") ||
    die "no local tag $tag: git tag $tag && git push origin $tag, then wait for the release workflow"
  [ "$tag_commit" = "$commit" ] || die "HEAD is not $tag (${tag_commit:0:12}): check out the tag so the APK matches it"
  git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null || die "tag $tag is not pushed: git push origin $tag"
  assets=$(gh release view "$tag" --repo "$repo" --json assets --jq '.assets[].name') ||
    die "no release $tag yet: wait for the release workflow (gh run list --repo $repo --workflow release.yml)"
  for a in "${server_assets[@]}" SHA256SUMS; do
    grep -qx "$a" <<<"$assets" || die "release $tag has no $a yet: wait for the release workflow"
  done
fi

rm -rf "$dist"
mkdir -p "$dist"
names=()
if [ "$apk" = 1 ]; then
  bash scripts/build-android.sh
  # Prefer a release build; a debug-signed APK still installs for personal use.
  built=$(find android/app/build/outputs/apk -name '*.apk' -path '*release*' 2>/dev/null | head -n1)
  if [ -z "$built" ]; then
    built=$(find android/app/build/outputs/apk -name '*.apk' 2>/dev/null | head -n1)
    [ -n "$built" ] && say "warning: no release APK, shipping $built"
  fi
  [ -n "$built" ] || die "no APK under android/app/build/outputs/apk"
  cp "$built" "$dist/magnetar-$tag.apk"
  names+=("magnetar-$tag.apk")
fi
for f in "${extra_files[@]}"; do
  cp "$f" "$dist/"
  names+=("$(basename "$f")")
done
[ "${#names[@]}" -gt 0 ] || die "nothing to upload"
(cd "$dist" && sha256sum "${names[@]}") >"$dist/SHA256SUMS.local"
say ""
say "dist/:"
(cd "$dist" && ls -lA)
say ""
cat "$dist/SHA256SUMS.local"

if [ "$mode" = build ]; then
  say ""
  say "Built. Publish with: scripts/release.sh --publish${suffix:+ --suffix $suffix}"
  exit 0
fi

# SHA256SUMS: keep CI's lines for the server binaries, replace ours.
gh release download "$tag" --repo "$repo" --pattern SHA256SUMS --output "$dist/SHA256SUMS.ci" --clobber
{
  awk 'NR == FNR { ours[$0]; next } !($2 in ours)' <(printf '%s\n' "${names[@]}") "$dist/SHA256SUMS.ci"
  cat "$dist/SHA256SUMS.local"
} | sort -k2 >"$dist/SHA256SUMS"
rm "$dist/SHA256SUMS.ci"
say ""
say "SHA256SUMS:"
cat "$dist/SHA256SUMS"
(cd "$dist" && gh release upload "$tag" --repo "$repo" --clobber "${names[@]}" SHA256SUMS)

notes_file=$notes
if [ -z "$notes_file" ]; then
  notes_file=$(mktemp)
  trap 'rm -f "$notes_file"' EXIT
  {
    echo "## Changes"
    git log --pretty='- %s' ${prev:+"$prev..HEAD"}
    echo
    echo "Install: docs/DEPLOY.md (image ghcr.io/$repo:${tag#v}, or the static Linux binaries)."
    echo "Verify downloads with SHA256SUMS."
  } >"$notes_file"
fi
gh release edit "$tag" --repo "$repo" --title "Magnetar $tag" --notes-file "$notes_file" \
  --draft=false --prerelease="$([ "$prerelease" = 1 ] && echo true || echo false)"
say ""
say "Published: https://github.com/$repo/releases/tag/$tag"

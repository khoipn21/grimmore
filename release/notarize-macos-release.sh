#!/usr/bin/env bash
set -euo pipefail
umask 077

die() {
  printf 'notarize macOS release: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: notarize-macos-release.sh --release <grimmore-release-*.zip> \
  --keychain-profile <notarytool-profile>

Submits the final ZIP to Apple's notary service and waits for an accepted
result. Credentials stay in the caller's Keychain profile, never in release
assets or this repository.
EOF
}

release_archive=""
profile=""

while (($# > 0)); do
  case "$1" in
    --release | --keychain-profile)
      (($# >= 2)) || die "missing value for $1"
      case "$1" in
        --release) release_archive="$2" ;;
        --keychain-profile) profile="$2" ;;
      esac
      shift 2
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      die "unknown option: $1"
      ;;
  esac
done

[[ -n "$release_archive" && -n "$profile" ]] || {
  usage >&2
  die "release archive and keychain profile are required"
}
[[ -f "$release_archive" && ! -L "$release_archive" ]] || die "release archive is not a regular file"
[[ "$(/usr/bin/basename "$release_archive")" =~ ^grimmore-release-[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?-macos-(x64|arm64)\.zip$ ]] || \
  die "release archive must use the expected portable macOS release name"
command -v xcrun >/dev/null 2>&1 || die "xcrun is unavailable"
xcrun notarytool submit "$release_archive" --wait --keychain-profile "$profile"
printf 'Apple notarization accepted %s\n' "$release_archive"

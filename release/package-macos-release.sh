#!/usr/bin/env bash
set -euo pipefail
umask 077

readonly ENVELOPE_NAME="Grimmore Release Envelope.app"
readonly PAYLOAD_NAME="grimmore-payload.zip"

die() {
  printf 'package macOS release: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: package-macos-release.sh --envelope <Grimmore Release Envelope.app> \
  --payload <grimmore-payload.zip> --out <grimmore-release-<version>-<target>.zip>

Packages the signed envelope and manifest-bound payload into the ZIP submitted
to Apple's notary service. Run notarize-macos-release.sh on this output before
passing it to the installer.
EOF
}

envelope=""
payload=""
output=""

while (($# > 0)); do
  case "$1" in
    --envelope | --payload | --out)
      (($# >= 2)) || die "missing value for $1"
      case "$1" in
        --envelope) envelope="$2" ;;
        --payload) payload="$2" ;;
        --out) output="$2" ;;
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

[[ -n "$envelope" && -n "$payload" && -n "$output" ]] || {
  usage >&2
  die "envelope, payload, and output are required"
}
[[ -d "$envelope" && ! -L "$envelope" ]] || die "envelope is not a real app directory"
[[ "$(/usr/bin/basename "$envelope")" == "$ENVELOPE_NAME" ]] || die "envelope uses an unexpected name"
[[ -f "$payload" && ! -L "$payload" ]] || die "payload is not a regular file"
[[ "$(/usr/bin/basename "$payload")" == "$PAYLOAD_NAME" ]] || die "payload uses an unexpected name"
[[ "$(/usr/bin/basename "$output")" =~ ^grimmore-release-[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?-macos-(x64|arm64)\.zip$ ]] || \
  die "output must use the expected portable macOS release name"
[[ ! -e "$output" && ! -L "$output" ]] || die "output already exists"
[[ -d "$(/usr/bin/dirname "$output")" ]] || die "output parent directory does not exist"
output="$(cd -P "$(/usr/bin/dirname "$output")" && pwd)/$(/usr/bin/basename "$output")"
/usr/bin/codesign --verify --strict --all-architectures "$envelope"

workspace="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/grimmore-macos-release.XXXXXX")"
cleanup() {
  /bin/rm -rf "$workspace"
}
trap cleanup EXIT
/usr/bin/ditto "$envelope" "$workspace/$ENVELOPE_NAME"
/bin/cp -p "$payload" "$workspace/$PAYLOAD_NAME"
(
  cd "$workspace"
  /usr/bin/zip -q -r -X "$output" "$ENVELOPE_NAME" "$PAYLOAD_NAME"
)
printf 'Created macOS release at %s\n' "$output"

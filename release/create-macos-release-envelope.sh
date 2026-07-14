#!/usr/bin/env bash
set -euo pipefail
umask 077

readonly ENVELOPE_IDENTIFIER="dev.grimmore.release-envelope"
readonly MAX_MANIFEST_BYTES=65536

die() {
  printf 'create macOS release envelope: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: create-macos-release-envelope.sh --manifest <release-manifest.json> \
  --launcher <signed-grimmore-launcher> --identity <Developer-ID-identity> \
  --version <normalized-version> --out <Grimmore Release Envelope.app>

Creates a data-only release envelope. The signed app's Resources directory
contains the exact manifest bytes; installers verify the app's pinned Developer
ID signature and notarization before reading those bytes.
EOF
}

manifest=""
launcher=""
identity=""
version=""
output=""

while (($# > 0)); do
  case "$1" in
    --manifest | --launcher | --identity | --version | --out)
      (($# >= 2)) || die "missing value for $1"
      case "$1" in
        --manifest) manifest="$2" ;;
        --launcher) launcher="$2" ;;
        --identity) identity="$2" ;;
        --version) version="$2" ;;
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

[[ -n "$manifest" && -n "$launcher" && -n "$identity" && -n "$version" && -n "$output" ]] || {
  usage >&2
  die "manifest, launcher, identity, version, and output are required"
}
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || \
  die "version must be normalized semantic version text"
[[ -f "$manifest" && ! -L "$manifest" ]] || die "manifest is not a regular file"
[[ -f "$launcher" && ! -L "$launcher" ]] || die "launcher is not a regular file"
[[ "$(/usr/bin/stat -f '%z' "$manifest")" -ge 1 && \
  "$(/usr/bin/stat -f '%z' "$manifest")" -le "$MAX_MANIFEST_BYTES" ]] || \
  die "manifest exceeds the permitted size"
[[ ! -e "$output" && ! -L "$output" ]] || die "output already exists"
[[ -d "$(/usr/bin/dirname "$output")" ]] || die "output parent directory does not exist"

/bin/mkdir "$output"
cleanup() {
  if [[ -d "$output" && ! -L "$output" ]]; then
    /bin/rm -rf "$output"
  fi
}
trap cleanup ERR

/bin/mkdir -p \
  "$output/Contents/MacOS" \
  "$output/Contents/Resources"
cat >"$output/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>grimmore-release-envelope</string>
  <key>CFBundleIdentifier</key>
  <string>${ENVELOPE_IDENTIFIER}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Grimmore Release Envelope</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleVersion</key>
  <string>${version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
</dict>
</plist>
EOF
/bin/cp -p "$launcher" "$output/Contents/MacOS/grimmore-release-envelope"
/bin/cp -p "$manifest" "$output/Contents/Resources/release-manifest.json"
/bin/chmod 755 "$output/Contents/MacOS/grimmore-release-envelope"
/bin/chmod 644 "$output/Contents/Info.plist" "$output/Contents/Resources/release-manifest.json"

# Sign nested code explicitly, then sign the bundle that hashes the manifest
# resource. Do not use --deep: Apple documents it as unsuitable for signing.
/usr/bin/codesign --force --options runtime --timestamp --sign "$identity" \
  --identifier "$ENVELOPE_IDENTIFIER" \
  "$output/Contents/MacOS/grimmore-release-envelope"
/usr/bin/codesign --force --options runtime --timestamp --sign "$identity" \
  --identifier "$ENVELOPE_IDENTIFIER" \
  "$output"
/usr/bin/codesign --verify --strict --all-architectures "$output"
trap - ERR
printf 'Created signed macOS release envelope at %s\n' "$output"

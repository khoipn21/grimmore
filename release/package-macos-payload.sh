#!/usr/bin/env bash
set -euo pipefail
umask 077

die() {
  printf 'package macOS payload: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: package-macos-payload.sh --daemon <grimmored> --launcher <grimmore-launcher> \
  --target <macos-x64|macos-arm64> --version <normalized-version> --out <payload.zip>

The binaries must already have their Developer ID signatures. This command only
creates the exact two-file payload ZIP hashed by the release manifest.
EOF
}

daemon=""
launcher=""
target=""
version=""
output=""

while (($# > 0)); do
  case "$1" in
    --daemon | --launcher | --target | --version | --out)
      (($# >= 2)) || die "missing value for $1"
      case "$1" in
        --daemon) daemon="$2" ;;
        --launcher) launcher="$2" ;;
        --target) target="$2" ;;
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

[[ -n "$daemon" && -n "$launcher" && -n "$target" && -n "$version" && -n "$output" ]] || {
  usage >&2
  die "daemon, launcher, target, version, and output are required"
}
[[ "$target" == "macos-x64" || "$target" == "macos-arm64" ]] || die "target must be a supported macOS target"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || \
  die "version must be normalized semantic version text"
[[ -f "$daemon" && ! -L "$daemon" && -f "$launcher" && ! -L "$launcher" ]] || \
  die "payload binaries must be regular files"
[[ ! -e "$output" && ! -L "$output" ]] || die "output already exists"
[[ -d "$(/usr/bin/dirname "$output")" ]] || die "output parent directory does not exist"
output="$(cd -P "$(/usr/bin/dirname "$output")" && pwd)/$(/usr/bin/basename "$output")"

case "$target" in
  macos-x64) expected_architecture="x86_64" ;;
  macos-arm64) expected_architecture="arm64" ;;
esac
for binary in "$daemon" "$launcher"; do
  architectures="$(/usr/bin/lipo -archs "$binary" 2>/dev/null)" || \
    die "payload binary is not a Mach-O executable: $binary"
  [[ "$architectures" == "$expected_architecture" ]] || \
    die "payload binary does not match target $target: $binary"
done

workspace="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/grimmore-macos-payload.XXXXXX")"
cleanup() {
  /bin/rm -rf "$workspace"
}
trap cleanup EXIT
payload_root="grimmore-$version-$target"
/bin/mkdir "$workspace/$payload_root"
/bin/cp -p "$daemon" "$workspace/$payload_root/grimmored"
/bin/cp -p "$launcher" "$workspace/$payload_root/grimmore-launcher"
/bin/chmod 700 "$workspace/$payload_root/grimmored" "$workspace/$payload_root/grimmore-launcher"
/usr/bin/codesign --verify --strict --all-architectures "$workspace/$payload_root/grimmored"
/usr/bin/codesign --verify --strict --all-architectures "$workspace/$payload_root/grimmore-launcher"
(
  cd "$workspace"
  /usr/bin/zip -q -r -X "$output" "$payload_root"
)
printf 'Created macOS payload at %s\n' "$output"

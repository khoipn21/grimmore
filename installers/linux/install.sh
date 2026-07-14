#!/usr/bin/env bash
set -euo pipefail
umask 077

readonly RELEASE_SCHEMA="https://grimmore.dev/schemas/release-manifest-v1.json"
readonly TEST_CHANNEL="test"

die() {
  printf 'grimmore installer: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: install.sh --archive <artifact.tar.gz> --manifest <manifest.json> \
  --signature <manifest.sig> --keyring <test-signer-keyring.gpg> \
  [--install-root <per-user directory>]

This Phase-1 installer accepts only the test signing channel. Production signing,
metadata freshness, and platform package verification are Phase-10 release gates.
EOF
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is unavailable: $1"
}

default_install_root() {
  if [[ -n "${XDG_DATA_HOME:-}" ]]; then
    printf '%s/grimmore\n' "$XDG_DATA_HOME"
  else
    printf '%s/.local/share/grimmore\n' "$HOME"
  fi
}

validate_install_root() {
  local root="$1"
  local current_uid
  current_uid="$(id -u)"
  mkdir -p -- "$root" "$root/versions" "$root/bin"
  for directory in "$root" "$root/versions" "$root/bin"; do
    [[ -d "$directory" && ! -L "$directory" ]] || \
      die "installation directory is not a real directory: $directory"
    [[ "$(stat -c '%u' -- "$directory")" == "$current_uid" ]] || \
      die "installation directory is not owned by the current user: $directory"
    chmod 700 -- "$directory"
  done
}

host_target() {
  local architecture
  case "$(uname -m)" in
    x86_64 | amd64) architecture="x64" ;;
    aarch64 | arm64) architecture="arm64" ;;
    *) die "unsupported Linux architecture: $(uname -m)" ;;
  esac
  getconf GNU_LIBC_VERSION 2>/dev/null | grep -q '^glibc ' || \
    die "this Phase-1 installer supports glibc Linux only"

  [[ -r /etc/os-release ]] || die "cannot verify the supported Ubuntu baseline"
  local os_id os_version
  os_id="$(awk -F= '$1 == "ID" { gsub(/"/, "", $2); print $2 }' /etc/os-release)"
  os_version="$(awk -F= '$1 == "VERSION_ID" { gsub(/"/, "", $2); print $2 }' /etc/os-release)"
  [[ "$os_id" == "ubuntu" && "$os_version" =~ ^([0-9]+)\.([0-9]+)$ ]] || \
    die "this Phase-1 installer supports Ubuntu 24.04 LTS or newer only"
  local major="${BASH_REMATCH[1]}"
  local minor="${BASH_REMATCH[2]}"
  ((major > 24 || (major == 24 && minor >= 4))) || \
    die "this Phase-1 installer supports Ubuntu 24.04 LTS or newer only"
  printf 'linux-%s-gnu\n' "$architecture"
}

set_link() {
  local root="$1"
  local name="$2"
  local target="$3"
  local temporary="$root/.${name}.$$"
  rm -f -- "$temporary"
  ln -s -- "$target" "$temporary"
  mv -Tf -- "$temporary" "$root/$name"
}

pointer_target() {
  local root="$1"
  local name="$2"
  local pointer="$root/$name"
  [[ -L "$pointer" ]] || return 1
  local target
  target="$(readlink -- "$pointer")"
  [[ "$target" =~ ^versions/[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || return 1
  [[ -d "$root/$target" && -f "$root/$target/.ready" ]] || return 1
  printf '%s\n' "$target"
}

validate_doctor_report() {
  local report="$1"
  python3 - "$report" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as source:
        value = json.load(source)
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"invalid staged doctor report: {error}")
protocol_version = value.get("protocolVersion") if isinstance(value, dict) else None
if (
    not isinstance(value, dict)
    or value.get("fts5Available") is not True
    or not isinstance(protocol_version, int)
    or isinstance(protocol_version, bool)
    or protocol_version != 1
    or value.get("credentialStoreAvailable") is not True
):
    raise SystemExit("staged companion failed its SQLite, credential-store, or protocol health check")
PY
}

ensure_stable_paths() {
  local root="$1"
  local command stable_path
  for command in grimmored grimmore-launcher; do
    stable_path="$root/bin/$command"
    if [[ -e "$stable_path" || -L "$stable_path" ]]; then
      [[ -L "$stable_path" && "$(readlink -- "$stable_path")" == "../current/$command" ]] || \
        die "stable launcher path already belongs to another installation: $stable_path"
    else
      ln -s -- "../current/$command" "$stable_path"
    fi
  done
}

validate_ready_version() {
  local directory="$1"
  local transaction="$2"
  cmp --silent -- "$transaction/manifest.json" "$directory/release-manifest.json" || return 1
  cmp --silent -- "$transaction/manifest.sig" "$directory/release-manifest.sig" || return 1
  [[ -x "$directory/grimmored" && -x "$directory/grimmore-launcher" ]] || return 1
  "$directory/grimmored" doctor >"$transaction/ready-doctor.json" \
    2>"$transaction/ready-doctor.stderr" || return 1
  validate_doctor_report "$transaction/ready-doctor.json" || return 1
  rm -f -- "$transaction/ready-doctor.json" "$transaction/ready-doctor.stderr"
}

archive=""
manifest=""
signature=""
keyring=""
install_root=""

while (($# > 0)); do
  case "$1" in
    --archive | --manifest | --signature | --keyring | --install-root)
      (($# >= 2)) || die "missing value for $1"
      case "$1" in
        --archive) archive="$2" ;;
        --manifest) manifest="$2" ;;
        --signature) signature="$2" ;;
        --keyring) keyring="$2" ;;
        --install-root) install_root="$2" ;;
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

[[ -n "$archive" && -n "$manifest" && -n "$signature" && -n "$keyring" ]] || {
  usage >&2
  die "archive, manifest, signature, and keyring are required"
}

for command in awk cmp flock gpgv id python3 sha256sum stat; do
  require_command "$command"
done
for input in "$archive" "$manifest" "$signature" "$keyring"; do
  [[ -f "$input" ]] || die "input is not a regular file: $input"
done

install_root="${install_root:-$(default_install_root)}"
validate_install_root "$install_root"
exec {lock_fd}>"$install_root/.install.lock"
flock -x "$lock_fd"

transaction_directory="$(mktemp -d "$install_root/.install.XXXXXX")"
staging_directory=""
version_directory=""
created_version=0
ready_version=0
committed=0
cleanup() {
  [[ -n "$staging_directory" ]] && rm -rf -- "$staging_directory"
  [[ -n "$transaction_directory" ]] && rm -rf -- "$transaction_directory"
  if ((created_version == 1 && ready_version == 0 && committed == 0)); then
    rm -rf -- "$version_directory"
  fi
}
trap cleanup EXIT

cp -- "$manifest" "$transaction_directory/manifest.json"
cp -- "$signature" "$transaction_directory/manifest.sig"
cp -- "$keyring" "$transaction_directory/signer-keyring.gpg"
cp -- "$archive" "$transaction_directory/artifact-input"
chmod 600 -- "$transaction_directory"/*

expected_target="$(host_target)"
gpgv --quiet --keyring "$transaction_directory/signer-keyring.gpg" \
  "$transaction_directory/manifest.sig" "$transaction_directory/manifest.json" \
  >/dev/null 2>&1 || die "manifest signature verification failed"

mapfile -t manifest_fields < <(python3 - "$transaction_directory/manifest.json" \
  "$expected_target" "$TEST_CHANNEL" "$RELEASE_SCHEMA" <<'PY'
import json
import re
import sys

manifest_path, expected_target, expected_channel, release_schema = sys.argv[1:]
try:
    with open(manifest_path, encoding="utf-8") as source:
        manifest = json.load(source)
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"invalid manifest: {error}")

expected_keys = {
    "$schema", "schemaVersion", "channel", "version", "target", "createdAt", "artifact", "protocol"
}
if not isinstance(manifest, dict) or set(manifest) != expected_keys:
    raise SystemExit("manifest has missing or unknown top-level fields")
if manifest["$schema"] != release_schema or manifest["schemaVersion"] != 1:
    raise SystemExit("manifest schema is unsupported")
if manifest["channel"] != expected_channel:
    raise SystemExit("manifest is not signed for the Phase-1 test channel")
if manifest["target"] != expected_target:
    raise SystemExit("manifest target does not match this host")
if not isinstance(manifest["version"], str) or not re.fullmatch(
    r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", manifest["version"]
):
    raise SystemExit("manifest version is invalid")
if not isinstance(manifest["createdAt"], str) or not re.fullmatch(
    r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z", manifest["createdAt"]
):
    raise SystemExit("manifest timestamp is invalid")

artifact = manifest["artifact"]
if not isinstance(artifact, dict) or set(artifact) != {"file", "sha256", "size"}:
    raise SystemExit("manifest artifact is invalid")
if not isinstance(artifact["file"], str) or not re.fullmatch(
    r"[A-Za-z0-9][A-Za-z0-9._-]*\.tar\.gz", artifact["file"]
):
    raise SystemExit("Linux artifact must have a portable .tar.gz name")
if not isinstance(artifact["sha256"], str) or not re.fullmatch(r"[a-f0-9]{64}", artifact["sha256"]):
    raise SystemExit("manifest artifact hash is invalid")
if not isinstance(artifact["size"], int) or isinstance(artifact["size"], bool) or artifact["size"] < 1:
    raise SystemExit("manifest artifact size is invalid")

protocol = manifest["protocol"]
if not isinstance(protocol, dict) or set(protocol) != {"minimum", "maximum"}:
    raise SystemExit("manifest protocol range is invalid")
minimum = protocol["minimum"]
maximum = protocol["maximum"]
if (
    not isinstance(minimum, int)
    or isinstance(minimum, bool)
    or not isinstance(maximum, int)
    or isinstance(maximum, bool)
    or not 1 <= minimum <= maximum <= 65535
):
    raise SystemExit("manifest protocol range is invalid")

print(manifest["version"])
print(artifact["file"])
print(artifact["sha256"])
print(artifact["size"])
PY
) || die "manifest validation failed"

((${#manifest_fields[@]} == 4)) || die "manifest parser returned an unexpected result"
version="${manifest_fields[0]}"
artifact_name="${manifest_fields[1]}"
artifact_sha256="${manifest_fields[2]}"
artifact_size="${manifest_fields[3]}"

[[ "$(basename -- "$archive")" == "$artifact_name" ]] || \
  die "artifact filename does not match the signed manifest"
[[ "$(wc -c < "$transaction_directory/artifact-input" | tr -d '[:space:]')" == "$artifact_size" ]] || \
  die "artifact size does not match the signed manifest"
[[ "$(sha256sum -- "$transaction_directory/artifact-input" | awk '{print $1}')" == "$artifact_sha256" ]] || \
  die "artifact hash does not match the signed manifest"

version_directory="$install_root/versions/$version"
if [[ -e "$version_directory" || -L "$version_directory" ]]; then
  [[ -d "$version_directory" && ! -L "$version_directory" ]] || \
    die "existing version path is unsafe: $version_directory"
  if [[ -f "$version_directory/.ready" ]]; then
    validate_ready_version "$version_directory" "$transaction_directory" || \
      die "existing version does not match the verified release payload"
    ready_version=1
  else
    rm -rf -- "$version_directory"
  fi
fi

if ((ready_version == 0)); then
  staging_directory="$(mktemp -d "$install_root/versions/.staging-$version.XXXXXX")"
  payload_directory="grimmore-$version-$expected_target"
  python3 - "$transaction_directory/artifact-input" "$staging_directory" "$payload_directory" <<'PY'
import os
from pathlib import Path, PurePosixPath
import shutil
import sys
import tarfile

archive_path = Path(sys.argv[1])
staging_directory = Path(sys.argv[2])
payload_directory = sys.argv[3]
expected_members = {
    f"{payload_directory}/": "directory",
    f"{payload_directory}/grimmored": "file",
    f"{payload_directory}/grimmore-launcher": "file",
}

try:
    archive = tarfile.open(archive_path, mode="r:gz")
except (OSError, tarfile.TarError) as error:
    raise SystemExit(f"cannot read artifact archive: {error}")

with archive:
    actual_members = {}
    for member in archive.getmembers():
        path = PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts or path.parts[0] != payload_directory:
            raise SystemExit("artifact contains an unsafe archive path")
        if member.isdir():
            kind = "directory"
        elif member.isfile():
            kind = "file"
        else:
            raise SystemExit("artifact may not contain links or special files")
        normalized_name = (
            f"{payload_directory}/"
            if kind == "directory" and member.name == payload_directory
            else member.name
        )
        if normalized_name not in expected_members or normalized_name in actual_members:
            raise SystemExit("artifact has an unexpected or duplicate archive member")
        if expected_members[normalized_name] != kind:
            raise SystemExit("artifact member has the wrong type")
        actual_members[normalized_name] = member
    if set(actual_members) != set(expected_members):
        raise SystemExit("artifact is missing a required payload member")

    payload = staging_directory / payload_directory
    payload.mkdir(mode=0o700)
    for name in (f"{payload_directory}/grimmored", f"{payload_directory}/grimmore-launcher"):
        member = actual_members[name]
        source = archive.extractfile(member)
        if source is None:
            raise SystemExit("artifact payload could not be read")
        destination = payload / PurePosixPath(name).name
        with source, destination.open("xb") as output:
            shutil.copyfileobj(source, output)
        os.chmod(destination, member.mode & 0o700)
PY

  [[ -x "$staging_directory/$payload_directory/grimmored" ]] || \
    die "artifact daemon is not executable"
  [[ -x "$staging_directory/$payload_directory/grimmore-launcher" ]] || \
    die "artifact stable launcher is not executable"
  "$staging_directory/$payload_directory/grimmored" doctor \
    >"$transaction_directory/staged-doctor.json" \
    2>"$transaction_directory/staged-doctor.stderr" || \
    die "staged companion failed its health check"
  validate_doctor_report "$transaction_directory/staged-doctor.json" || \
    die "staged companion failed its health check"
  rm -f -- "$transaction_directory/staged-doctor.json" \
    "$transaction_directory/staged-doctor.stderr"

  mv -- "$staging_directory/$payload_directory" "$version_directory"
  created_version=1
  cp -- "$transaction_directory/manifest.json" "$version_directory/release-manifest.json"
  cp -- "$transaction_directory/manifest.sig" "$version_directory/release-manifest.sig"
  chmod -R go-rwx -- "$version_directory"
  : >"$version_directory/.ready"
  chmod 600 -- "$version_directory/.ready"
  ready_version=1
fi

ensure_stable_paths "$install_root"
current_target=""
if [[ -e "$install_root/current" || -L "$install_root/current" ]]; then
  current_target="$(pointer_target "$install_root" current)" || \
    die "existing current pointer is not a safe installed version"
fi
if [[ "$current_target" != "versions/$version" ]]; then
  if [[ -n "$current_target" ]]; then
    set_link "$install_root" previous "$current_target"
  fi
  set_link "$install_root" current "versions/$version"
fi
committed=1
printf 'Installed Grimmore %s for %s at %s\n' "$version" "$expected_target" "$install_root"

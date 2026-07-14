#!/usr/bin/env bash
set -euo pipefail
umask 077

die() {
  printf 'grimmore rollback: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: rollback.sh [--install-root <per-user directory>]

Roll back only to the single version retained by the signed-install pointer. This
is an explicit local recovery action; it never downloads or installs a version.
EOF
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
  for directory in "$root" "$root/versions" "$root/bin"; do
    [[ -d "$directory" && ! -L "$directory" ]] || \
      die "installation directory is not a real directory: $directory"
    [[ "$(stat -c '%u' -- "$directory")" == "$current_uid" ]] || \
      die "installation directory is not owned by the current user: $directory"
    chmod 700 -- "$directory"
  done
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

set_link() {
  local root="$1"
  local name="$2"
  local target="$3"
  local temporary="$root/.${name}.$$"
  rm -f -- "$temporary"
  ln -s -- "$target" "$temporary"
  mv -Tf -- "$temporary" "$root/$name"
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
    raise SystemExit(f"invalid rollback doctor report: {error}")
protocol_version = value.get("protocolVersion") if isinstance(value, dict) else None
if (
    not isinstance(value, dict)
    or value.get("fts5Available") is not True
    or not isinstance(protocol_version, int)
    or isinstance(protocol_version, bool)
    or protocol_version != 1
    or value.get("credentialStoreAvailable") is not True
):
    raise SystemExit("rollback companion failed its SQLite, credential-store, or protocol health check")
PY
}

install_root=""
while (($# > 0)); do
  case "$1" in
    --install-root)
      (($# >= 2)) || die "missing value for --install-root"
      install_root="$2"
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

for command in flock id mktemp python3 stat; do
  command -v "$command" >/dev/null 2>&1 || die "required command is unavailable: $command"
done
install_root="${install_root:-$(default_install_root)}"
validate_install_root "$install_root"
exec {lock_fd}>"$install_root/.install.lock"
flock -x "$lock_fd"
transaction_directory="$(mktemp -d "$install_root/.rollback.XXXXXX")"
cleanup() {
  rm -rf -- "$transaction_directory"
}
trap cleanup EXIT

current_target="$(pointer_target "$install_root" current)" || \
  die "current pointer is not a safe installed version"
previous_target="$(pointer_target "$install_root" previous)" || \
  die "no policy-permitted rollback version is available"
[[ -x "$install_root/$previous_target/grimmored" ]] || \
  die "rollback companion is not executable"
"$install_root/$previous_target/grimmored" doctor \
  >"$transaction_directory/doctor.json" \
  2>"$transaction_directory/doctor.stderr" || \
  die "rollback companion failed its health check"
validate_doctor_report "$transaction_directory/doctor.json" || \
  die "rollback companion failed its health check"

set_link "$install_root" current "$previous_target"
set_link "$install_root" previous "$current_target"
printf 'Rolled back to %s\n' "${previous_target#versions/}"

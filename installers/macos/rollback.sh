#!/usr/bin/env bash
set -euo pipefail
umask 077

readonly SCRIPT_DIRECTORY="$(cd -P "$(dirname "$0")" && pwd)"
# shellcheck source=release-common.sh
source "$SCRIPT_DIRECTORY/release-common.sh"

usage() {
  cat <<'EOF'
Usage: rollback.sh --trusted-certificate-sha1 <Developer-ID-certificate-SHA-1> \
  --trusted-team-id <Apple-team-id> [--install-root <per-user directory>]

Roll back only to the one installed version retained by the signed-install
pointer. This is an explicit local recovery action; it never downloads code.
EOF
}

certificate_sha1=""
team_id=""
install_root=""

while (($# > 0)); do
  case "$1" in
    --trusted-certificate-sha1 | --trusted-team-id | --install-root)
      (($# >= 2)) || grimmore_release_die "missing value for $1"
      case "$1" in
        --trusted-certificate-sha1) certificate_sha1="$2" ;;
        --trusted-team-id) team_id="$2" ;;
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
      grimmore_release_die "unknown option: $1"
      ;;
  esac
done

[[ -n "$certificate_sha1" && -n "$team_id" ]] || {
  usage >&2
  grimmore_release_die "trusted certificate and trusted team ID are required"
}

grimmore_require_macos_commands
grimmore_set_trusted_signer "$certificate_sha1" "$team_id"
target="$(grimmore_get_macos_target)"
install_root="$(grimmore_resolve_install_root "${install_root:-$(grimmore_default_install_root)}")"
grimmore_assert_existing_install_root "$install_root"
grimmore_enter_install_lock "$install_root"
transaction=""
cleanup() {
  if [[ -n "$transaction" ]]; then
    grimmore_remove_private_directory "$transaction" "$install_root"
  fi
  grimmore_leave_install_lock
}
trap cleanup EXIT

transaction="$(grimmore_new_private_directory "$install_root" ".rollback-")"
grimmore_recover_pointer_switch "$install_root" "$target" "$transaction"
current_target="$(grimmore_pointer_target "$install_root" current)" || \
  grimmore_release_die "current pointer is not a safe installed version"
previous_target="$(grimmore_pointer_target "$install_root" previous)" || \
  grimmore_release_die "no policy-permitted rollback version is available"
[[ "$current_target" != "$previous_target" ]] || \
  grimmore_release_die "rollback state does not identify a distinct prior version"
grimmore_validate_ready_version \
  "$install_root/$current_target" \
  "$target" \
  "${current_target#versions/}" \
  "" \
  "" \
  "$transaction"
grimmore_validate_ready_version \
  "$install_root/$previous_target" \
  "$target" \
  "${previous_target#versions/}" \
  "" \
    "" \
    "$transaction"
grimmore_ensure_stable_paths "$install_root"
grimmore_switch_pointer_pair "$install_root" "$current_target" "$previous_target"
printf 'Rolled back to %s\n' "${previous_target#versions/}"

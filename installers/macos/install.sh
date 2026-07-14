#!/usr/bin/env bash
set -euo pipefail
umask 077

readonly SCRIPT_DIRECTORY="$(cd -P "$(dirname "$0")" && pwd)"
# shellcheck source=release-common.sh
source "$SCRIPT_DIRECTORY/release-common.sh"

usage() {
  cat <<'EOF'
Usage: install.sh --release <notarized-release.zip> \
  --trusted-certificate-sha1 <Developer-ID-certificate-SHA-1> \
  --trusted-team-id <Apple-team-id> [--install-root <per-user directory>]

This Phase-1 installer accepts only the test signing channel. It never imports
a release certificate: the caller supplies a pinned Apple Developer ID leaf
certificate and team identifier already trusted by macOS.
EOF
}

release_archive=""
certificate_sha1=""
team_id=""
install_root=""

while (($# > 0)); do
  case "$1" in
    --release | --trusted-certificate-sha1 | --trusted-team-id | --install-root)
      (($# >= 2)) || grimmore_release_die "missing value for $1"
      case "$1" in
        --release) release_archive="$2" ;;
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

[[ -n "$release_archive" && -n "$certificate_sha1" && -n "$team_id" ]] || {
  usage >&2
  grimmore_release_die "release archive, trusted certificate, and trusted team ID are required"
}

grimmore_require_macos_commands
grimmore_assert_regular_file "$release_archive" "release archive"
[[ "$(/usr/bin/basename "$release_archive")" =~ ^grimmore-release-[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?-macos-(x64|arm64)\.zip$ ]] || \
  grimmore_release_die "release archive must use the expected portable macOS release name"
[[ "$(grimmore_file_size "$release_archive")" -le "$GRIMMORE_MAX_RELEASE_ARCHIVE_BYTES" ]] || \
  grimmore_release_die "release archive exceeds the permitted size"

grimmore_set_trusted_signer "$certificate_sha1" "$team_id"
target="$(grimmore_get_macos_target)"
install_root="$(grimmore_resolve_install_root "${install_root:-$(grimmore_default_install_root)}")"
grimmore_initialize_install_root "$install_root"
grimmore_enter_install_lock "$install_root"

transaction=""
staging=""
committed=0
cleanup() {
  if [[ -n "$staging" ]]; then
    grimmore_remove_private_directory "$staging" "$install_root/versions"
  fi
  if [[ -n "$transaction" ]]; then
    grimmore_remove_private_directory "$transaction" "$install_root"
  fi
  grimmore_leave_install_lock
}
trap cleanup EXIT

transaction="$(grimmore_new_private_directory "$install_root" ".install-")"
grimmore_recover_pointer_switch "$install_root" "$target" "$transaction"
transaction_archive="$transaction/release.zip"
grimmore_copy_bounded_file \
  "$release_archive" \
  "$transaction_archive" \
  "release archive" \
  "$GRIMMORE_MAX_RELEASE_ARCHIVE_BYTES"
grimmore_assert_outer_release_archive "$transaction_archive" "$transaction/outer-entries.txt"
release_contents="$transaction/release"
/bin/mkdir "$release_contents"
grimmore_assert_owned_private_directory "$release_contents" "release transaction directory"
grimmore_extract_zip "$transaction_archive" "$release_contents"
envelope="$release_contents/$GRIMMORE_ENVELOPE_NAME"
payload_archive="$release_contents/$GRIMMORE_PAYLOAD_ARCHIVE_NAME"
grimmore_harden_release_envelope_tree "$envelope"
grimmore_assert_regular_file "$payload_archive" "release payload archive"
grimmore_clear_acl "$payload_archive"
/bin/chmod 600 "$payload_archive"
[[ "$(grimmore_file_size "$payload_archive")" -le "$GRIMMORE_MAX_PAYLOAD_ARCHIVE_BYTES" ]] || \
  grimmore_release_die "release payload archive exceeds the permitted size"
grimmore_assert_signed_notarized "$envelope" "release envelope" "$GRIMMORE_SIGNING_REQUIREMENT"
manifest="$envelope/Contents/Resources/release-manifest.json"
grimmore_read_release_manifest "$manifest" "$target"
[[ "$GRIMMORE_MANIFEST_ARTIFACT_FILE" == "$GRIMMORE_PAYLOAD_ARCHIVE_NAME" ]] || \
  grimmore_release_die "signed manifest does not identify the expected payload archive"
[[ "$(grimmore_file_size "$payload_archive")" == "$GRIMMORE_MANIFEST_ARTIFACT_SIZE" ]] || \
  grimmore_release_die "release payload archive size does not match the signed manifest"
[[ "$(grimmore_sha256 "$payload_archive")" == "$GRIMMORE_MANIFEST_ARTIFACT_SHA256" ]] || \
  grimmore_release_die "release payload archive hash does not match the signed manifest"

version="$GRIMMORE_MANIFEST_VERSION"
version_directory="$install_root/versions/$version"
if [[ -e "$version_directory" || -L "$version_directory" ]]; then
  grimmore_validate_ready_version \
    "$version_directory" \
    "$target" \
    "$version" \
    "$manifest" \
    "$envelope" \
    "$transaction"
else
  staging="$(grimmore_new_private_directory "$install_root/versions" ".staging-$version-")"
  payload_root="grimmore-$version-$target"
  grimmore_assert_payload_archive "$payload_archive" "$transaction/payload-entries.txt" "$payload_root"
  payload_contents="$staging/payload"
  /bin/mkdir "$payload_contents"
  grimmore_assert_owned_private_directory "$payload_contents" "staged payload directory"
  grimmore_extract_zip "$payload_archive" "$payload_contents"
  grimmore_assert_real_directory "$payload_contents/$payload_root" "staged payload directory"
  /bin/mv "$payload_contents/$payload_root/grimmored" "$staging/grimmored"
  /bin/mv "$payload_contents/$payload_root/grimmore-launcher" "$staging/grimmore-launcher"
  /bin/rmdir "$payload_contents/$payload_root"
  /bin/rmdir "$payload_contents"
  grimmore_assert_regular_file "$staging/grimmored" "staged companion"
  grimmore_assert_regular_file "$staging/grimmore-launcher" "staged versioned launcher"
  grimmore_clear_acl "$staging/grimmored"
  grimmore_clear_acl "$staging/grimmore-launcher"
  /bin/chmod 700 "$staging/grimmored" "$staging/grimmore-launcher"
  grimmore_assert_macos_binary_target "$staging/grimmored" "$target" "staged companion"
  grimmore_assert_macos_binary_target "$staging/grimmore-launcher" "$target" "staged versioned launcher"
  grimmore_assert_signed_notarized "$staging/grimmored" "staged companion" "$GRIMMORE_BINARY_REQUIREMENT"
  grimmore_assert_signed_notarized "$staging/grimmore-launcher" "staged versioned launcher" "$GRIMMORE_BINARY_REQUIREMENT"
  /bin/cp -p "$manifest" "$staging/release-manifest.json"
  grimmore_clear_acl "$staging/release-manifest.json"
  /bin/chmod 600 "$staging/release-manifest.json"
  grimmore_copy_release_envelope "$envelope" "$staging/release-envelope.app"
  grimmore_assert_doctor_report \
    "$staging/grimmored" \
    "$transaction/staged-doctor.stderr" \
    "$GRIMMORE_MANIFEST_PROTOCOL_MINIMUM" \
    "$GRIMMORE_MANIFEST_PROTOCOL_MAXIMUM" \
    "$transaction/staged-doctor.json"
  : >"$staging/.ready"
  grimmore_clear_acl "$staging/.ready"
  /bin/chmod 600 "$staging/.ready"
  grimmore_sync_ready_version_tree "$staging"
  /bin/mv "$staging" "$version_directory"
  grimmore_sync_directory_with_full_barrier \
    "$install_root/versions" \
    "$version_directory/.ready"
  staging=""
  grimmore_validate_ready_version \
    "$version_directory" \
    "$target" \
    "$version" \
    "$manifest" \
    "$envelope" \
    "$transaction"
fi

grimmore_ensure_stable_paths "$install_root"
current_target=""
if [[ -e "$install_root/current" || -L "$install_root/current" ]]; then
  current_target="$(grimmore_pointer_target "$install_root" current)" || \
    grimmore_release_die "existing current pointer is not a safe installed version"
  grimmore_validate_ready_version \
    "$install_root/$current_target" \
    "$target" \
    "${current_target#versions/}" \
    "" \
    "" \
    "$transaction"
elif [[ -e "$install_root/previous" || -L "$install_root/previous" ]]; then
  grimmore_release_die "previous pointer exists without a current pointer"
fi
if [[ "$current_target" != "versions/$version" ]]; then
  if [[ -n "$current_target" ]]; then
    grimmore_switch_pointer_pair "$install_root" "$current_target" "versions/$version"
  else
    grimmore_set_pointer "$install_root" current "versions/$version"
  fi
fi
committed=1
printf 'Installed Grimmore %s for %s at %s\n' "$version" "$target" "$install_root"

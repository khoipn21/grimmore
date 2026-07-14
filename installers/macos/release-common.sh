#!/usr/bin/env bash

# Shared fail-closed helpers for the macOS per-user release scripts. This file
# is sourced by install.sh and rollback.sh; it intentionally supports the
# system Bash shipped with macOS 14 rather than requiring Homebrew tooling.

readonly GRIMMORE_RELEASE_SCHEMA="https://grimmore.dev/schemas/release-manifest-v1.json"
readonly GRIMMORE_TEST_CHANNEL="test"
readonly GRIMMORE_MAX_RELEASE_ARCHIVE_BYTES=$((768 * 1024 * 1024))
readonly GRIMMORE_MAX_PAYLOAD_ARCHIVE_BYTES=$((512 * 1024 * 1024))
readonly GRIMMORE_MAX_PAYLOAD_EXPANDED_BYTES=$((512 * 1024 * 1024))
readonly GRIMMORE_MAX_ENVELOPE_EXECUTABLE_BYTES=$((256 * 1024 * 1024))
readonly GRIMMORE_MAX_MANIFEST_BYTES=65536
readonly GRIMMORE_MAX_INFO_PLIST_BYTES=65536
readonly GRIMMORE_MAX_CODE_RESOURCES_BYTES=$((1024 * 1024))
readonly GRIMMORE_ENVELOPE_NAME="Grimmore Release Envelope.app"
readonly GRIMMORE_PAYLOAD_ARCHIVE_NAME="grimmore-payload.zip"
readonly GRIMMORE_ENVELOPE_IDENTIFIER="dev.grimmore.release-envelope"

grimmore_release_die() {
  printf 'grimmore release: %s\n' "$*" >&2
  exit 1
}

grimmore_require_command() {
  command -v "$1" >/dev/null 2>&1 || \
    grimmore_release_die "required command is unavailable: $1"
}

grimmore_require_macos_commands() {
  local command
  for command in codesign ditto find id lipo lockf mktemp perl readlink shasum spctl stat sw_vers sysctl unzip zipinfo; do
    grimmore_require_command "$command"
  done
  [[ -x /usr/bin/osascript ]] || \
    grimmore_release_die "the macOS JSON parser is unavailable: /usr/bin/osascript"
}

grimmore_default_install_root() {
  [[ -n "${HOME:-}" ]] || \
    grimmore_release_die "HOME is unavailable for the current user"
  printf '%s/Library/Application Support/Grimmore\n' "$HOME"
}

grimmore_resolve_install_root() {
  local root="$1"
  [[ "$root" == /* ]] || \
    grimmore_release_die "installation root must be an absolute path"
  [[ "$root" != *$'\n'* && "$root" != *$'\r'* ]] || \
    grimmore_release_die "installation root contains an unsupported character"
  [[ "$root" != *"//"* ]] || \
    grimmore_release_die "installation root must not contain repeated path separators"
  while [[ "$root" != "/" && "$root" == */ ]]; do
    root="${root%/}"
  done
  [[ "$root" != "/" && -n "$root" ]] || \
    grimmore_release_die "installation root must not be the filesystem root"
  case "/$root/" in
    */../* | */./*) grimmore_release_die "installation root must not contain dot path components" ;;
  esac
  printf '%s\n' "$root"
}

grimmore_assert_regular_file() {
  local path="$1"
  local description="$2"
  [[ -f "$path" && ! -L "$path" ]] || \
    grimmore_release_die "$description is not a regular file: $path"
}

grimmore_assert_real_directory() {
  local path="$1"
  local description="$2"
  [[ -d "$path" && ! -L "$path" ]] || \
    grimmore_release_die "$description is not a real directory: $path"
}

grimmore_file_size() {
  /usr/bin/stat -f '%z' "$1"
}

grimmore_clear_acl() {
  /bin/chmod -N "$1" || \
    grimmore_release_die "cannot remove inherited access-control entries: $1"
}

grimmore_sync_regular_file() {
  local path="$1"
  grimmore_assert_regular_file "$path" "durable release file"
  /usr/bin/perl -MIO::Handle -MFcntl=F_FULLFSYNC -e '
    use strict;
    use warnings;
    my $path = shift @ARGV;
    open my $file, "+<", $path or die "open $path: $!\n";
    $file->sync() or die "fsync $path: $!\n";
    defined fcntl($file, F_FULLFSYNC, 0) or die "full fsync $path: $!\n";
    close $file or die "close $path: $!\n";
  ' "$path" >/dev/null 2>&1 || \
    grimmore_release_die "cannot durably synchronize release file: $path"
}

grimmore_sync_directory() {
  local path="$1"
  grimmore_assert_real_directory "$path" "durable release directory"
  /usr/bin/perl -MIO::Handle -MFcntl=O_RDONLY -e '
    use strict;
    use warnings;
    my $path = shift @ARGV;
    sysopen(my $directory, $path, O_RDONLY) or die "open directory $path: $!\n";
    $directory->sync() or die "fsync directory $path: $!\n";
    close $directory or die "close directory $path: $!\n";
  ' "$path" >/dev/null 2>&1 || \
    grimmore_release_die "cannot durably synchronize release directory: $path"
}

grimmore_sync_directory_with_full_barrier() {
  local directory="$1"
  local barrier_file="$2"
  local directory_device barrier_device

  grimmore_assert_real_directory "$directory" "durable release directory"
  grimmore_assert_regular_file "$barrier_file" "release metadata barrier file"
  directory_device="$(/usr/bin/stat -f '%d' "$directory")"
  barrier_device="$(/usr/bin/stat -f '%d' "$barrier_file")"
  [[ -n "$directory_device" && "$directory_device" == "$barrier_device" ]] || \
    grimmore_release_die "release metadata barrier is not on the directory filesystem"

  # On APFS, a directory fsync writes the rename/unlink metadata, while a
  # same-device F_FULLFSYNC guarantees all earlier fsync'd I/O is persistent.
  # Use a regular protected file as the full-sync descriptor because the
  # platform's durable barrier contract is documented for files, not folders.
  grimmore_sync_directory "$directory"
  grimmore_sync_regular_file "$barrier_file"
}

grimmore_assert_owned_private_directory() {
  local path="$1"
  local description="$2"
  local current_uid
  current_uid="$(/usr/bin/id -u)"
  grimmore_assert_real_directory "$path" "$description"
  [[ "$(/usr/bin/stat -f '%u' "$path")" == "$current_uid" ]] || \
    grimmore_release_die "$description is not owned by the current user: $path"
  grimmore_clear_acl "$path"
  /bin/chmod 700 "$path"
}

grimmore_assert_owned_private_file() {
  local path="$1"
  local description="$2"
  local current_uid
  current_uid="$(/usr/bin/id -u)"
  grimmore_assert_regular_file "$path" "$description"
  [[ "$(/usr/bin/stat -f '%u' "$path")" == "$current_uid" ]] || \
    grimmore_release_die "$description is not owned by the current user: $path"
  grimmore_clear_acl "$path"
  /bin/chmod 600 "$path"
}

grimmore_initialize_install_root() {
  local root="$1"
  /bin/mkdir -p "$root" "$root/versions" "$root/bin"
  grimmore_assert_owned_private_directory "$root" "installation root"
  grimmore_assert_owned_private_directory "$root/versions" "installed versions directory"
  grimmore_assert_owned_private_directory "$root/bin" "stable launcher directory"
}

grimmore_assert_existing_install_root() {
  local root="$1"
  grimmore_assert_owned_private_directory "$root" "installation root"
  grimmore_assert_owned_private_directory "$root/versions" "installed versions directory"
  grimmore_assert_owned_private_directory "$root/bin" "stable launcher directory"
}

grimmore_enter_install_lock() {
  local root="$1"
  local lock="$root/.install.lock"

  if [[ -e "$lock" || -L "$lock" ]]; then
    grimmore_assert_owned_private_file "$lock" "installation lock"
  fi
  # lockf keeps an advisory BSD lock on this shell's file descriptor. Unlike a
  # PID directory, a forced termination releases it automatically and cannot
  # leave a stale lock that requires manual deletion.
  exec 9>"$lock"
  grimmore_assert_owned_private_file "$lock" "installation lock"
  /usr/bin/lockf -s -t 0 9 || \
    grimmore_release_die "another installation or rollback is already running"
  GRIMMORE_INSTALL_LOCK="$lock"
}

grimmore_leave_install_lock() {
  exec 9>&- 2>/dev/null || true
  GRIMMORE_INSTALL_LOCK=""
}

grimmore_new_private_directory() {
  local parent="$1"
  local prefix="$2"
  local directory
  directory="$(/usr/bin/mktemp -d "$parent/$prefix"'XXXXXX')" || \
    grimmore_release_die "cannot create a private transaction directory"
  grimmore_assert_owned_private_directory "$directory" "private transaction directory"
  printf '%s\n' "$directory"
}

grimmore_remove_private_directory() {
  local path="$1"
  local parent="$2"
  [[ -n "$path" ]] || return
  case "$path" in
    "$parent"/.install-* | "$parent"/.rollback-* | "$parent"/.staging-*) ;;
    *) grimmore_release_die "refusing to remove an unexpected transaction path" ;;
  esac
  if [[ -e "$path" || -L "$path" ]]; then
    grimmore_assert_real_directory "$path" "private transaction directory"
    /bin/rm -rf "$path"
  fi
}

grimmore_copy_bounded_file() {
  local source="$1"
  local destination="$2"
  local description="$3"
  local maximum_bytes="$4"
  local source_size destination_size

  grimmore_assert_regular_file "$source" "$description"
  source_size="$(grimmore_file_size "$source")"
  [[ "$source_size" =~ ^[0-9]+$ && "$source_size" -ge 1 && "$source_size" -le "$maximum_bytes" ]] || \
    grimmore_release_die "$description exceeds the permitted size"
  /bin/cp -p "$source" "$destination"
  grimmore_assert_regular_file "$destination" "copied $description"
  destination_size="$(grimmore_file_size "$destination")"
  [[ "$destination_size" == "$source_size" ]] || \
    grimmore_release_die "copied $description changed while it was read"
  grimmore_clear_acl "$destination"
  /bin/chmod 600 "$destination"
}

grimmore_sha256() {
  /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{print $1}'
}

grimmore_get_macos_target() {
  local product_version major architecture translated
  product_version="$(/usr/bin/sw_vers -productVersion)"
  major="${product_version%%.*}"
  [[ "$major" =~ ^[0-9]+$ && "$major" -ge 14 ]] || \
    grimmore_release_die "this Phase-1 installer supports macOS 14 or newer only"
  translated="$(/usr/sbin/sysctl -in sysctl.proc_translated 2>/dev/null || true)"
  [[ "$translated" != "1" ]] || \
    grimmore_release_die "run the installer from a native architecture shell, not Rosetta"
  architecture="$(/usr/bin/uname -m)"
  case "$architecture" in
    x86_64) printf 'macos-x64\n' ;;
    arm64) printf 'macos-arm64\n' ;;
    *) grimmore_release_die "unsupported macOS processor architecture: $architecture" ;;
  esac
}

grimmore_assert_macos_binary_target() {
  local path="$1"
  local target="$2"
  local description="$3"
  local expected_architecture architectures

  case "$target" in
    macos-x64) expected_architecture="x86_64" ;;
    macos-arm64) expected_architecture="arm64" ;;
    *) grimmore_release_die "unsupported macOS release target: $target" ;;
  esac
  grimmore_assert_regular_file "$path" "$description"
  architectures="$(/usr/bin/lipo -archs "$path" 2>/dev/null)" || \
    grimmore_release_die "$description is not a Mach-O binary"
  [[ "$architectures" == "$expected_architecture" ]] || \
    grimmore_release_die "$description does not match the signed release target"
}

grimmore_set_trusted_signer() {
  local certificate_sha1="$1"
  local team_id="$2"
  certificate_sha1="$(printf '%s' "$certificate_sha1" | /usr/bin/tr 'A-F' 'a-f')"
  [[ "$certificate_sha1" =~ ^[a-f0-9]{40}$ ]] || \
    grimmore_release_die "trusted signing certificate must be a SHA-1 certificate hash"
  [[ "$team_id" =~ ^[A-Z0-9]{10}$ ]] || \
    grimmore_release_die "trusted signing team identifier is invalid"
  GRIMMORE_TRUSTED_CERTIFICATE_SHA1="$certificate_sha1"
  GRIMMORE_TRUSTED_TEAM_ID="$team_id"
  GRIMMORE_SIGNING_REQUIREMENT="identifier \"$GRIMMORE_ENVELOPE_IDENTIFIER\" and anchor apple generic and certificate leaf = H\"$certificate_sha1\" and certificate leaf[subject.OU] = \"$team_id\""
  GRIMMORE_BINARY_REQUIREMENT="anchor apple generic and certificate leaf = H\"$certificate_sha1\" and certificate leaf[subject.OU] = \"$team_id\""
}

grimmore_assert_signed_notarized() {
  local path="$1"
  local description="$2"
  local requirement="$3"

  /usr/bin/codesign --verify --strict --all-architectures \
    --test-requirement "$requirement" "$path" >/dev/null 2>&1 || \
    grimmore_release_die "$description does not have the pinned Developer ID signature"
  /usr/bin/codesign --verify --strict --all-architectures --check-notarization \
    --test-requirement "$requirement" "$path" >/dev/null 2>&1 || \
    grimmore_release_die "$description is not notarized by Apple"
  /usr/sbin/spctl --assess --type execute --verbose=4 "$path" >/dev/null 2>&1 || \
    grimmore_release_die "$description is not accepted by macOS Gatekeeper"
}

grimmore_zip_entry_size() {
  local archive="$1"
  local entry="$2"
  local listing size_lines size attributes
  listing="$(LC_ALL=C /usr/bin/zipinfo -l "$archive" "$entry")" || \
    grimmore_release_die "cannot inspect release archive member: $entry"
  size_lines="$(printf '%s\n' "$listing" | /usr/bin/awk 'NF >= 9 && $4 ~ /^[0-9]+$/ { print $4 ":" $1 }')"
  [[ "$(printf '%s\n' "$size_lines" | /usr/bin/awk 'NF { count += 1 } END { print count + 0 }')" == "1" ]] || \
    grimmore_release_die "release archive has invalid metadata for: $entry"
  size="${size_lines%%:*}"
  attributes="${size_lines#*:}"
  [[ "$size" =~ ^[0-9]+$ && "${attributes:0:1}" == "-" ]] || \
    grimmore_release_die "release archive member is not a regular file: $entry"
  printf '%s\n' "$size"
}

grimmore_assert_zip_directory() {
  local archive="$1"
  local entry="$2"
  local listing metadata_lines size attributes
  listing="$(LC_ALL=C /usr/bin/zipinfo -l "$archive" "$entry")" || \
    grimmore_release_die "cannot inspect release archive directory: $entry"
  metadata_lines="$(printf '%s\n' "$listing" | /usr/bin/awk 'NF >= 9 && $4 ~ /^[0-9]+$/ { print $4 ":" $1 }')"
  [[ "$(printf '%s\n' "$metadata_lines" | /usr/bin/awk 'NF { count += 1 } END { print count + 0 }')" == "1" ]] || \
    grimmore_release_die "release archive has invalid metadata for directory: $entry"
  size="${metadata_lines%%:*}"
  attributes="${metadata_lines#*:}"
  [[ "$size" == 0 && "${attributes:0:1}" == "d" ]] || \
    grimmore_release_die "release archive member is not a directory: $entry"
}

grimmore_assert_outer_release_archive() {
  local archive="$1"
  local listing="$2"
  local entry
  local root_count=0 contents_count=0 macos_count=0 resources_directory_count=0
  local signature_directory_count=0 info_count=0 executable_count=0 manifest_count=0
  local resources_count=0 payload_count=0
  local envelope="$GRIMMORE_ENVELOPE_NAME"

  LC_ALL=C /usr/bin/unzip -Z1 "$archive" >"$listing" || \
    grimmore_release_die "cannot inspect the release archive"
  while IFS= read -r entry || [[ -n "$entry" ]]; do
    case "$entry" in
      "$envelope/") root_count=$((root_count + 1)) ;;
      "$envelope/Contents/") contents_count=$((contents_count + 1)) ;;
      "$envelope/Contents/MacOS/") macos_count=$((macos_count + 1)) ;;
      "$envelope/Contents/Resources/") resources_directory_count=$((resources_directory_count + 1)) ;;
      "$envelope/Contents/_CodeSignature/") signature_directory_count=$((signature_directory_count + 1)) ;;
      "$envelope/Contents/Info.plist") info_count=$((info_count + 1)) ;;
      "$envelope/Contents/MacOS/grimmore-release-envelope") executable_count=$((executable_count + 1)) ;;
      "$envelope/Contents/Resources/release-manifest.json") manifest_count=$((manifest_count + 1)) ;;
      "$envelope/Contents/_CodeSignature/CodeResources") resources_count=$((resources_count + 1)) ;;
      "$GRIMMORE_PAYLOAD_ARCHIVE_NAME") payload_count=$((payload_count + 1)) ;;
      *) grimmore_release_die "release archive has an unexpected or unsafe member" ;;
    esac
  done <"$listing"
  [[ "$info_count" == 1 && "$executable_count" == 1 && "$manifest_count" == 1 && \
    "$resources_count" == 1 && "$payload_count" == 1 ]] || \
    grimmore_release_die "release archive is missing or duplicates a required member"
  [[ "$root_count" -le 1 && "$contents_count" -le 1 && "$macos_count" -le 1 && \
    "$resources_directory_count" -le 1 && "$signature_directory_count" -le 1 ]] || \
    grimmore_release_die "release archive has a duplicate directory member"
  if [[ "$root_count" == 1 ]]; then
    grimmore_assert_zip_directory "$archive" "$envelope/"
  fi
  if [[ "$contents_count" == 1 ]]; then
    grimmore_assert_zip_directory "$archive" "$envelope/Contents/"
  fi
  if [[ "$macos_count" == 1 ]]; then
    grimmore_assert_zip_directory "$archive" "$envelope/Contents/MacOS/"
  fi
  if [[ "$resources_directory_count" == 1 ]]; then
    grimmore_assert_zip_directory "$archive" "$envelope/Contents/Resources/"
  fi
  if [[ "$signature_directory_count" == 1 ]]; then
    grimmore_assert_zip_directory "$archive" "$envelope/Contents/_CodeSignature/"
  fi

  local size
  size="$(grimmore_zip_entry_size "$archive" "$envelope/Contents/Info.plist")"
  [[ "$size" -le "$GRIMMORE_MAX_INFO_PLIST_BYTES" ]] || \
    grimmore_release_die "release envelope information file exceeds the permitted size"
  size="$(grimmore_zip_entry_size "$archive" "$envelope/Contents/MacOS/grimmore-release-envelope")"
  [[ "$size" -ge 1 && "$size" -le "$GRIMMORE_MAX_ENVELOPE_EXECUTABLE_BYTES" ]] || \
    grimmore_release_die "release envelope executable exceeds the permitted size"
  size="$(grimmore_zip_entry_size "$archive" "$envelope/Contents/Resources/release-manifest.json")"
  [[ "$size" -ge 1 && "$size" -le "$GRIMMORE_MAX_MANIFEST_BYTES" ]] || \
    grimmore_release_die "release manifest exceeds the permitted size"
  size="$(grimmore_zip_entry_size "$archive" "$envelope/Contents/_CodeSignature/CodeResources")"
  [[ "$size" -ge 1 && "$size" -le "$GRIMMORE_MAX_CODE_RESOURCES_BYTES" ]] || \
    grimmore_release_die "release envelope signature resources exceed the permitted size"
  size="$(grimmore_zip_entry_size "$archive" "$GRIMMORE_PAYLOAD_ARCHIVE_NAME")"
  [[ "$size" -ge 1 && "$size" -le "$GRIMMORE_MAX_PAYLOAD_ARCHIVE_BYTES" ]] || \
    grimmore_release_die "release payload archive exceeds the permitted size"
}

grimmore_assert_payload_archive() {
  local archive="$1"
  local listing="$2"
  local payload_root="$3"
  local entry root_count=0 daemon_count=0 launcher_count=0 total_size=0 size

  LC_ALL=C /usr/bin/unzip -Z1 "$archive" >"$listing" || \
    grimmore_release_die "cannot inspect the payload archive"
  while IFS= read -r entry || [[ -n "$entry" ]]; do
    case "$entry" in
      "$payload_root/") root_count=$((root_count + 1)) ;;
      "$payload_root/grimmored") daemon_count=$((daemon_count + 1)) ;;
      "$payload_root/grimmore-launcher") launcher_count=$((launcher_count + 1)) ;;
      *) grimmore_release_die "payload archive has an unexpected or unsafe member" ;;
    esac
  done <"$listing"
  [[ "$daemon_count" == 1 && "$launcher_count" == 1 ]] || \
    grimmore_release_die "payload archive is missing or duplicates a required member"
  [[ "$root_count" -le 1 ]] || \
    grimmore_release_die "payload archive has a duplicate directory member"
  if [[ "$root_count" == 1 ]]; then
    grimmore_assert_zip_directory "$archive" "$payload_root/"
  fi

  size="$(grimmore_zip_entry_size "$archive" "$payload_root/grimmored")"
  [[ "$size" -ge 1 ]] || grimmore_release_die "payload companion is empty"
  total_size=$((total_size + size))
  size="$(grimmore_zip_entry_size "$archive" "$payload_root/grimmore-launcher")"
  [[ "$size" -ge 1 ]] || grimmore_release_die "payload launcher is empty"
  total_size=$((total_size + size))
  [[ "$total_size" -le "$GRIMMORE_MAX_PAYLOAD_EXPANDED_BYTES" ]] || \
    grimmore_release_die "payload archive expands beyond the permitted size"
}

grimmore_extract_zip() {
  local archive="$1"
  local destination="$2"
  /usr/bin/ditto -x -k "$archive" "$destination" || \
    grimmore_release_die "cannot extract the verified release archive"
}

grimmore_assert_release_envelope_tree() {
  local envelope="$1"
  local entry relative
  local info_count=0 executable_count=0 manifest_count=0 resources_count=0

  grimmore_assert_real_directory "$envelope" "release envelope"
  while IFS= read -r entry || [[ -n "$entry" ]]; do
    [[ "$entry" == "$envelope" ]] && continue
    relative="${entry#"$envelope"/}"
    case "$relative" in
      Contents | Contents/MacOS | Contents/Resources | Contents/_CodeSignature)
        grimmore_assert_real_directory "$entry" "release envelope directory"
        ;;
      Contents/Info.plist)
        info_count=$((info_count + 1))
        grimmore_assert_regular_file "$entry" "release envelope information file"
        ;;
      Contents/MacOS/grimmore-release-envelope)
        executable_count=$((executable_count + 1))
        grimmore_assert_regular_file "$entry" "release envelope executable"
        ;;
      Contents/Resources/release-manifest.json)
        manifest_count=$((manifest_count + 1))
        grimmore_assert_regular_file "$entry" "release manifest"
        ;;
      Contents/_CodeSignature/CodeResources)
        resources_count=$((resources_count + 1))
        grimmore_assert_regular_file "$entry" "release envelope signature resources"
        ;;
      *) grimmore_release_die "release envelope has an unexpected or unsafe member" ;;
    esac
  done < <(/usr/bin/find -x "$envelope" -print)
  [[ "$info_count" == 1 && "$executable_count" == 1 && "$manifest_count" == 1 && \
    "$resources_count" == 1 ]] || \
    grimmore_release_die "release envelope is missing or duplicates a required member"

  local path size
  for path in \
    "$envelope/Contents/Info.plist" \
    "$envelope/Contents/MacOS/grimmore-release-envelope" \
    "$envelope/Contents/Resources/release-manifest.json" \
    "$envelope/Contents/_CodeSignature/CodeResources"; do
    size="$(grimmore_file_size "$path")"
    [[ "$size" =~ ^[0-9]+$ ]] || grimmore_release_die "release envelope has an invalid file size"
  done
  [[ "$(grimmore_file_size "$envelope/Contents/Info.plist")" -le "$GRIMMORE_MAX_INFO_PLIST_BYTES" ]] || \
    grimmore_release_die "release envelope information file exceeds the permitted size"
  [[ "$(grimmore_file_size "$envelope/Contents/MacOS/grimmore-release-envelope")" -le "$GRIMMORE_MAX_ENVELOPE_EXECUTABLE_BYTES" ]] || \
    grimmore_release_die "release envelope executable exceeds the permitted size"
  [[ "$(grimmore_file_size "$envelope/Contents/Resources/release-manifest.json")" -le "$GRIMMORE_MAX_MANIFEST_BYTES" ]] || \
    grimmore_release_die "release manifest exceeds the permitted size"
  [[ "$(grimmore_file_size "$envelope/Contents/_CodeSignature/CodeResources")" -le "$GRIMMORE_MAX_CODE_RESOURCES_BYTES" ]] || \
    grimmore_release_die "release envelope signature resources exceed the permitted size"
}

grimmore_harden_release_envelope_tree() {
  local envelope="$1"
  local directory file
  local directories=("" "Contents" "Contents/MacOS" "Contents/Resources" "Contents/_CodeSignature")
  local private_files=(
    "Contents/Info.plist"
    "Contents/Resources/release-manifest.json"
    "Contents/_CodeSignature/CodeResources"
  )

  grimmore_assert_release_envelope_tree "$envelope"
  for directory in "${directories[@]}"; do
    grimmore_clear_acl "$envelope${directory:+/$directory}"
    /bin/chmod 700 "$envelope${directory:+/$directory}"
  done
  for file in "${private_files[@]}"; do
    grimmore_clear_acl "$envelope/$file"
    /bin/chmod 600 "$envelope/$file"
  done
  grimmore_clear_acl "$envelope/Contents/MacOS/grimmore-release-envelope"
  /bin/chmod 700 "$envelope/Contents/MacOS/grimmore-release-envelope"
  grimmore_assert_release_envelope_tree "$envelope"
}

grimmore_sync_ready_version_tree() {
  local version_directory="$1"
  local path
  local files=(
    "grimmored"
    "grimmore-launcher"
    "release-manifest.json"
    "release-envelope.app/Contents/Info.plist"
    "release-envelope.app/Contents/MacOS/grimmore-release-envelope"
    "release-envelope.app/Contents/Resources/release-manifest.json"
    "release-envelope.app/Contents/_CodeSignature/CodeResources"
    ".ready"
  )
  local directories=(
    "release-envelope.app/Contents/MacOS"
    "release-envelope.app/Contents/Resources"
    "release-envelope.app/Contents/_CodeSignature"
    "release-envelope.app/Contents"
    "release-envelope.app"
    "."
  )

  for path in "${files[@]}"; do
    grimmore_sync_regular_file "$version_directory/$path"
  done
  for path in "${directories[@]}"; do
    grimmore_sync_directory "$version_directory/$path"
  done
  # The directory syncs above only submit metadata.  The final full flush of
  # the ready marker persists their prior same-device I/O before the staging
  # directory is renamed into the visible versions directory.
  grimmore_sync_regular_file "$version_directory/.ready"
}

grimmore_copy_release_envelope() {
  local source="$1"
  local destination="$2"
  local relative
  local directories=("Contents" "Contents/MacOS" "Contents/Resources" "Contents/_CodeSignature")
  local files=(
    "Contents/Info.plist"
    "Contents/MacOS/grimmore-release-envelope"
    "Contents/Resources/release-manifest.json"
    "Contents/_CodeSignature/CodeResources"
  )

  grimmore_harden_release_envelope_tree "$source"
  /bin/mkdir "$destination"
  grimmore_clear_acl "$destination"
  /bin/chmod 700 "$destination"
  for relative in "${directories[@]}"; do
    /bin/mkdir "$destination/$relative"
    grimmore_clear_acl "$destination/$relative"
    /bin/chmod 700 "$destination/$relative"
  done
  for relative in "${files[@]}"; do
    /bin/cp -p "$source/$relative" "$destination/$relative"
  done
  grimmore_harden_release_envelope_tree "$destination"
}

grimmore_read_release_manifest() {
  local manifest="$1"
  local expected_target="$2"
  local output line
  local fields=()

  grimmore_assert_regular_file "$manifest" "release manifest"
  [[ "$(grimmore_file_size "$manifest")" -le "$GRIMMORE_MAX_MANIFEST_BYTES" ]] || \
    grimmore_release_die "release manifest exceeds the permitted size"
  if ! output="$(GRIMMORE_MANIFEST_PATH="$manifest" GRIMMORE_EXPECTED_TARGET="$expected_target" \
    /usr/bin/osascript -l JavaScript - <<'JXA'
ObjC.import('Foundation');

function fail(message) {
  throw new Error(message);
}

function environmentValue(name) {
  const value = $.NSProcessInfo.processInfo.environment.objectForKey($(name));
  if (value === undefined || value.isNil()) {
    fail(`missing ${name}`);
  }
  return ObjC.unwrap(value);
}

function exactObject(value, keys, description) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    fail(`${description} is not an object`);
  }
  const actual = Object.keys(value).sort();
  const expected = keys.slice().sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    fail(`${description} has missing or unknown fields`);
  }
}

function validInteger(value, minimum, maximum) {
  return typeof value === 'number' && isFinite(value) && Math.floor(value) === value &&
    value >= minimum && value <= maximum;
}

try {
  const manifestPath = environmentValue('GRIMMORE_MANIFEST_PATH');
  const expectedTarget = environmentValue('GRIMMORE_EXPECTED_TARGET');
  const data = $.NSData.dataWithContentsOfFile($(manifestPath));
  if (data === undefined || data.isNil()) {
    fail('cannot read release manifest');
  }
  const text = ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding));
  const manifest = JSON.parse(text);
  exactObject(manifest, ['$schema', 'schemaVersion', 'channel', 'version', 'target', 'createdAt', 'artifact', 'protocol'], 'release manifest');
  if (manifest.$schema !== 'https://grimmore.dev/schemas/release-manifest-v1.json' || manifest.schemaVersion !== 1) {
    fail('release manifest schema is unsupported');
  }
  if (manifest.channel !== 'test' || manifest.target !== expectedTarget) {
    fail('release manifest channel or target is invalid');
  }
  if (typeof manifest.version !== 'string' || !/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version)) {
    fail('release manifest version is invalid');
  }
  if (typeof manifest.createdAt !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$/.test(manifest.createdAt) || Number.isNaN(Date.parse(manifest.createdAt))) {
    fail('release manifest timestamp is invalid');
  }
  exactObject(manifest.artifact, ['file', 'sha256', 'size'], 'release manifest artifact');
  if (manifest.artifact.file !== 'grimmore-payload.zip' ||
      typeof manifest.artifact.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(manifest.artifact.sha256) ||
      !validInteger(manifest.artifact.size, 1, Number.MAX_SAFE_INTEGER)) {
    fail('release manifest artifact is invalid');
  }
  exactObject(manifest.protocol, ['minimum', 'maximum'], 'release manifest protocol');
  if (!validInteger(manifest.protocol.minimum, 1, 65535) ||
      !validInteger(manifest.protocol.maximum, 1, 65535) ||
      manifest.protocol.minimum > manifest.protocol.maximum) {
    fail('release manifest protocol range is invalid');
  }
  console.log([
    manifest.version,
    manifest.artifact.file,
    manifest.artifact.sha256,
    String(manifest.artifact.size),
    String(manifest.protocol.minimum),
    String(manifest.protocol.maximum),
  ].join('\n'));
} catch (error) {
  $.NSFileHandle.fileHandleWithStandardError.writeData(
    $(String(error) + '\n').dataUsingEncoding($.NSUTF8StringEncoding),
  );
  $.exit(1);
}
JXA
  )"; then
    grimmore_release_die "release manifest validation failed"
  fi
  while IFS= read -r line || [[ -n "$line" ]]; do
    fields+=("$line")
  done <<<"$output"
  [[ "${#fields[@]}" == 6 ]] || \
    grimmore_release_die "release manifest parser returned an unexpected result"
  GRIMMORE_MANIFEST_VERSION="${fields[0]}"
  GRIMMORE_MANIFEST_ARTIFACT_FILE="${fields[1]}"
  GRIMMORE_MANIFEST_ARTIFACT_SHA256="${fields[2]}"
  GRIMMORE_MANIFEST_ARTIFACT_SIZE="${fields[3]}"
  GRIMMORE_MANIFEST_PROTOCOL_MINIMUM="${fields[4]}"
  GRIMMORE_MANIFEST_PROTOCOL_MAXIMUM="${fields[5]}"
}

grimmore_assert_doctor_report() {
  local daemon="$1"
  local standard_error="$2"
  local protocol_minimum="$3"
  local protocol_maximum="$4"
  local report="$5"

  grimmore_assert_regular_file "$daemon" "staged companion"
  "$daemon" doctor >"$report" 2>"$standard_error" || \
    grimmore_release_die "staged companion failed its health check"
  [[ "$(grimmore_file_size "$report")" -ge 1 && \
    "$(grimmore_file_size "$report")" -le "$GRIMMORE_MAX_MANIFEST_BYTES" ]] || \
    grimmore_release_die "staged companion emitted an invalid health report size"
  if ! GRIMMORE_DOCTOR_PATH="$report" \
    GRIMMORE_PROTOCOL_MINIMUM="$protocol_minimum" \
    GRIMMORE_PROTOCOL_MAXIMUM="$protocol_maximum" \
      /usr/bin/osascript -l JavaScript - >/dev/null <<'JXA'
ObjC.import('Foundation');

function value(name) {
  const item = $.NSProcessInfo.processInfo.environment.objectForKey($(name));
  if (item === undefined || item.isNil()) {
    throw new Error(`missing ${name}`);
  }
  return ObjC.unwrap(item);
}

const data = $.NSData.dataWithContentsOfFile($(value('GRIMMORE_DOCTOR_PATH')));
if (data === undefined || data.isNil()) {
  throw new Error('cannot read doctor report');
}
const report = JSON.parse(ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding)));
const minimum = Number(value('GRIMMORE_PROTOCOL_MINIMUM'));
const maximum = Number(value('GRIMMORE_PROTOCOL_MAXIMUM'));
if (report === null || typeof report !== 'object' || Array.isArray(report) ||
    report.fts5Available !== true || report.credentialStoreAvailable !== true ||
    typeof report.protocolVersion !== 'number' || !isFinite(report.protocolVersion) ||
    Math.floor(report.protocolVersion) !== report.protocolVersion ||
    report.protocolVersion < minimum || report.protocolVersion > maximum ||
    report.platform !== 'macos') {
  throw new Error('invalid doctor report');
}
JXA
  then
    grimmore_release_die "staged companion failed its SQLite, Keychain, or protocol health check"
  fi
}

grimmore_validate_ready_version() {
  local version_directory="$1"
  local expected_target="$2"
  local expected_version="$3"
  local transaction_manifest="${4:-}"
  local transaction_envelope="${5:-}"
  local transaction_directory="$6"
  local manifest envelope daemon launcher

  grimmore_assert_owned_private_directory "$version_directory" "installed version directory"
  grimmore_assert_regular_file "$version_directory/.ready" "installed readiness marker"
  manifest="$version_directory/release-manifest.json"
  envelope="$version_directory/release-envelope.app"
  daemon="$version_directory/grimmored"
  launcher="$version_directory/grimmore-launcher"
  grimmore_assert_regular_file "$manifest" "stored release manifest"
  grimmore_clear_acl "$version_directory/.ready"
  /bin/chmod 600 "$version_directory/.ready"
  grimmore_clear_acl "$manifest"
  /bin/chmod 600 "$manifest"
  grimmore_harden_release_envelope_tree "$envelope"
  grimmore_clear_acl "$daemon"
  /bin/chmod 700 "$daemon"
  grimmore_clear_acl "$launcher"
  /bin/chmod 700 "$launcher"
  grimmore_assert_signed_notarized "$envelope" "release envelope" "$GRIMMORE_SIGNING_REQUIREMENT"
  /usr/bin/cmp -s "$manifest" "$envelope/Contents/Resources/release-manifest.json" || \
    grimmore_release_die "stored release manifest does not match its signed envelope"
  if [[ -n "$transaction_manifest" ]]; then
    /usr/bin/cmp -s "$manifest" "$transaction_manifest" || \
      grimmore_release_die "existing ready version has different signed release evidence"
  fi
  if [[ -n "$transaction_envelope" ]]; then
    grimmore_harden_release_envelope_tree "$transaction_envelope"
    /usr/bin/cmp -s \
      "$envelope/Contents/Resources/release-manifest.json" \
      "$transaction_envelope/Contents/Resources/release-manifest.json" || \
      grimmore_release_die "existing ready version has different signed release evidence"
  fi
  grimmore_read_release_manifest "$manifest" "$expected_target"
  [[ "$GRIMMORE_MANIFEST_VERSION" == "$expected_version" ]] || \
    grimmore_release_die "installed version does not match its signed release manifest"
  grimmore_assert_macos_binary_target "$daemon" "$expected_target" "installed companion"
  grimmore_assert_macos_binary_target "$launcher" "$expected_target" "installed versioned launcher"
  grimmore_assert_signed_notarized "$daemon" "installed companion" "$GRIMMORE_BINARY_REQUIREMENT"
  grimmore_assert_signed_notarized "$launcher" "installed versioned launcher" "$GRIMMORE_BINARY_REQUIREMENT"
  grimmore_assert_doctor_report \
    "$daemon" \
    "$transaction_directory/ready-doctor.stderr" \
    "$GRIMMORE_MANIFEST_PROTOCOL_MINIMUM" \
    "$GRIMMORE_MANIFEST_PROTOCOL_MAXIMUM" \
    "$transaction_directory/ready-doctor.json"
}

grimmore_pointer_target() {
  local root="$1"
  local name="$2"
  local pointer="$root/$name"
  local target
  [[ -L "$pointer" ]] || return 1
  target="$(/usr/bin/readlink "$pointer")"
  [[ "$target" =~ ^versions/[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || return 1
  [[ -d "$root/$target" && ! -L "$root/$target" && -f "$root/$target/.ready" && ! -L "$root/$target/.ready" ]] || return 1
  printf '%s\n' "$target"
}

grimmore_assert_pointer_target_path() {
  local root="$1"
  local target="$2"
  local description="$3"
  [[ "$target" =~ ^versions/[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || \
    grimmore_release_die "$description is not a safe installed version"
  [[ -d "$root/$target" && ! -L "$root/$target" && \
    -f "$root/$target/.ready" && ! -L "$root/$target/.ready" ]] || \
    grimmore_release_die "$description is not a ready installed version"
}

grimmore_set_pointer() {
  local root="$1"
  local name="$2"
  local target="$3"
  local temporary="$root/.${name}.$$.${RANDOM}"
  local barrier_file="${GRIMMORE_INSTALL_LOCK:-}"

  [[ -n "$barrier_file" ]] || \
    grimmore_release_die "installation lock is unavailable for a durable pointer switch"
  /bin/ln -s "$target" "$temporary"
  if [[ -e "$root/$name" || -L "$root/$name" ]]; then
    [[ -L "$root/$name" ]] || grimmore_release_die "existing $name pointer is not a symbolic link"
    /bin/mv -fh "$temporary" "$root/$name"
  else
    /bin/mv -f "$temporary" "$root/$name"
  fi
  grimmore_sync_directory_with_full_barrier "$root" "$barrier_file"
}

grimmore_write_pointer_switch_journal() {
  local root="$1"
  local from_target="$2"
  local to_target="$3"
  local journal="$root/.pointer-switch"
  local temporary="$root/.pointer-switch.$$.${RANDOM}.tmp"

  grimmore_assert_pointer_target_path "$root" "$from_target" "pointer switch source"
  grimmore_assert_pointer_target_path "$root" "$to_target" "pointer switch destination"
  [[ "$from_target" != "$to_target" ]] || \
    grimmore_release_die "pointer switch must select a distinct version"
  [[ ! -e "$journal" && ! -L "$journal" ]] || \
    grimmore_release_die "an unfinished pointer switch must be recovered first"
  printf 'schemaVersion=1\nfrom=%s\nto=%s\n' "$from_target" "$to_target" >"$temporary"
  grimmore_assert_owned_private_file "$temporary" "pointer switch journal"
  grimmore_sync_regular_file "$temporary"
  /bin/mv -f "$temporary" "$journal"
  grimmore_sync_directory_with_full_barrier "$root" "$journal"
}

grimmore_read_pointer_switch_journal() {
  local root="$1"
  local journal="$root/.pointer-switch"
  local line
  local lines=()

  [[ -e "$journal" || -L "$journal" ]] || return 1
  grimmore_assert_owned_private_file "$journal" "pointer switch journal"
  [[ "$(grimmore_file_size "$journal")" -ge 1 && \
    "$(grimmore_file_size "$journal")" -le 1024 ]] || \
    grimmore_release_die "pointer switch journal has an invalid size"
  while IFS= read -r line || [[ -n "$line" ]]; do
    lines+=("$line")
  done <"$journal"
  [[ "${#lines[@]}" == 3 && "${lines[0]}" == "schemaVersion=1" ]] || \
    grimmore_release_die "pointer switch journal is invalid"
  GRIMMORE_SWITCH_FROM="${lines[1]#from=}"
  GRIMMORE_SWITCH_TO="${lines[2]#to=}"
  [[ "from=$GRIMMORE_SWITCH_FROM" == "${lines[1]}" && \
    "to=$GRIMMORE_SWITCH_TO" == "${lines[2]}" ]] || \
    grimmore_release_die "pointer switch journal is invalid"
  grimmore_assert_pointer_target_path "$root" "$GRIMMORE_SWITCH_FROM" "pointer switch source"
  grimmore_assert_pointer_target_path "$root" "$GRIMMORE_SWITCH_TO" "pointer switch destination"
  [[ "$GRIMMORE_SWITCH_FROM" != "$GRIMMORE_SWITCH_TO" ]] || \
    grimmore_release_die "pointer switch journal does not select a distinct version"
}

grimmore_recover_pointer_switch() {
  local root="$1"
  local expected_target="${2:-}"
  local transaction_directory="${3:-}"
  local current_target

  if ! grimmore_read_pointer_switch_journal "$root"; then
    return
  fi
  current_target="$(grimmore_pointer_target "$root" current)" || \
    grimmore_release_die "current pointer cannot be recovered after an interrupted switch"
  [[ "$current_target" == "$GRIMMORE_SWITCH_FROM" || \
    "$current_target" == "$GRIMMORE_SWITCH_TO" ]] || \
    grimmore_release_die "current pointer conflicts with the interrupted switch journal"
  if [[ -n "$expected_target" || -n "$transaction_directory" ]]; then
    [[ -n "$expected_target" && -n "$transaction_directory" ]] || \
      grimmore_release_die "pointer switch recovery requires complete release validation context"
    grimmore_validate_ready_version \
      "$root/$GRIMMORE_SWITCH_FROM" \
      "$expected_target" \
      "${GRIMMORE_SWITCH_FROM#versions/}" \
      "" \
      "" \
      "$transaction_directory"
    grimmore_validate_ready_version \
      "$root/$GRIMMORE_SWITCH_TO" \
      "$expected_target" \
      "${GRIMMORE_SWITCH_TO#versions/}" \
      "" \
      "" \
      "$transaction_directory"
  fi
  grimmore_set_pointer "$root" previous "$GRIMMORE_SWITCH_FROM"
  grimmore_set_pointer "$root" current "$GRIMMORE_SWITCH_TO"
  /bin/rm -f "$root/.pointer-switch"
  grimmore_sync_directory_with_full_barrier "$root" "$GRIMMORE_INSTALL_LOCK"
}

grimmore_switch_pointer_pair() {
  local root="$1"
  local from_target="$2"
  local to_target="$3"
  local current_target

  current_target="$(grimmore_pointer_target "$root" current)" || \
    grimmore_release_die "current pointer is not a safe installed version"
  [[ "$current_target" == "$from_target" ]] || \
    grimmore_release_die "current pointer changed before the version switch"
  grimmore_write_pointer_switch_journal "$root" "$from_target" "$to_target"
  grimmore_set_pointer "$root" previous "$from_target"
  grimmore_set_pointer "$root" current "$to_target"
  /bin/rm -f "$root/.pointer-switch"
  grimmore_sync_directory_with_full_barrier "$root" "$GRIMMORE_INSTALL_LOCK"
}

grimmore_ensure_stable_paths() {
  local root="$1"
  local command stable_path
  local changed=0
  for command in grimmored grimmore-launcher; do
    stable_path="$root/bin/$command"
    if [[ -e "$stable_path" || -L "$stable_path" ]]; then
      [[ -L "$stable_path" && "$(/usr/bin/readlink "$stable_path")" == "../current/$command" ]] || \
        grimmore_release_die "stable launcher path already belongs to another installation: $stable_path"
    else
      /bin/ln -s "../current/$command" "$stable_path"
      changed=1
    fi
  done
  if [[ "$changed" == 1 ]]; then
    grimmore_sync_directory_with_full_barrier "$root/bin" "$GRIMMORE_INSTALL_LOCK"
  fi
}

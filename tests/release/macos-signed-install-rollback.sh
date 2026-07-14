#!/usr/bin/env bash
set -euo pipefail
umask 077

repository="$(cd -P "$(dirname "$0")/../.." && pwd)"
installer="$repository/installers/macos/install.sh"
rollback="$repository/installers/macos/rollback.sh"
manifest_builder="$repository/release/create-slice-manifest.mjs"
payload_builder="$repository/release/package-macos-payload.sh"
envelope_builder="$repository/release/create-macos-release-envelope.sh"
release_builder="$repository/release/package-macos-release.sh"
notarizer="$repository/release/notarize-macos-release.sh"

die() {
  printf 'macOS signed-install smoke: %s\n' "$*" >&2
  exit 1
}

[[ "$(/usr/bin/uname -s)" == "Darwin" ]] || \
  die "this smoke must run on a native macOS 14+ runner"
[[ -n "${GRIMMORE_MACOS_TEST_SIGNING_IDENTITY:-}" ]] || \
  die "GRIMMORE_MACOS_TEST_SIGNING_IDENTITY must name a provisioned Developer ID signing identity"
[[ "${GRIMMORE_MACOS_TEST_CERT_SHA1:-}" =~ ^[A-Fa-f0-9]{40}$ ]] || \
  die "GRIMMORE_MACOS_TEST_CERT_SHA1 must be the pinned Developer ID certificate SHA-1"
[[ "${GRIMMORE_MACOS_TEST_TEAM_ID:-}" =~ ^[A-Z0-9]{10}$ ]] || \
  die "GRIMMORE_MACOS_TEST_TEAM_ID must be the pinned Apple team ID"
[[ -n "${GRIMMORE_MACOS_NOTARY_PROFILE:-}" ]] || \
  die "GRIMMORE_MACOS_NOTARY_PROFILE must name a notarytool Keychain profile"

case "$(/usr/bin/uname -m)" in
  x86_64) target="macos-x64" ;;
  arm64) target="macos-arm64" ;;
  *) die "unsupported native macOS architecture" ;;
esac

for command in cargo codesign ditto node readlink xcrun zip; do
  command -v "$command" >/dev/null 2>&1 || die "required command is unavailable: $command"
done

workspace="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/grimmore-macos-release-smoke.XXXXXX")"
runtime_directory="$(/usr/bin/mktemp -d /tmp/grimmore-macos-runtime.XXXXXX)"
cleanup() {
  /bin/rm -rf "$workspace"
  /bin/rm -rf "$runtime_directory"
}
trap cleanup EXIT
/bin/chmod 700 "$runtime_directory"

cd "$repository"
cargo build --locked --release -p grimmored -p grimmore-launcher

make_release() {
  local version="$1"
  local release_directory="$workspace/$version"
  local daemon="$release_directory/grimmored"
  local launcher="$release_directory/grimmore-launcher"
  local payload="$release_directory/grimmore-payload.zip"
  local manifest="$release_directory/release-manifest.json"
  local envelope="$release_directory/Grimmore Release Envelope.app"
  local release="$release_directory/grimmore-release-$version-$target.zip"

  /bin/mkdir -p "$release_directory"
  /bin/cp -p "$repository/target/release/grimmored" "$daemon"
  /bin/cp -p "$repository/target/release/grimmore-launcher" "$launcher"
  /bin/chmod 700 "$daemon" "$launcher"
  /usr/bin/codesign --force --options runtime --timestamp \
    --sign "$GRIMMORE_MACOS_TEST_SIGNING_IDENTITY" \
    --identifier "dev.grimmore.release-test.$version.grimmored" "$daemon"
  /usr/bin/codesign --force --options runtime --timestamp \
    --sign "$GRIMMORE_MACOS_TEST_SIGNING_IDENTITY" \
    --identifier "dev.grimmore.release-test.$version.launcher" "$launcher"
  "$payload_builder" \
    --daemon "$daemon" \
    --launcher "$launcher" \
    --target "$target" \
    --version "$version" \
    --out "$payload" >&2
  node "$manifest_builder" \
    --artifact "$payload" \
    --channel test \
    --created-at 2026-07-13T00:00:00Z \
    --out "$manifest" \
    --target "$target" \
    --version "$version"
  "$envelope_builder" \
    --manifest "$manifest" \
    --launcher "$launcher" \
    --identity "$GRIMMORE_MACOS_TEST_SIGNING_IDENTITY" \
    --version "$version" \
    --out "$envelope" >&2
  "$release_builder" \
    --envelope "$envelope" \
    --payload "$payload" \
    --out "$release" >&2
  "$notarizer" \
    --release "$release" \
    --keychain-profile "$GRIMMORE_MACOS_NOTARY_PROFILE" >&2
  printf '%s\n' "$release"
}

assert_pointer() {
  local root="$1"
  local name="$2"
  local expected="$3"
  [[ -L "$root/$name" ]] || die "$name pointer is missing"
  [[ "$(/usr/bin/readlink "$root/$name")" == "versions/$expected" ]] || \
    die "$name pointer does not identify $expected"
}

replace_pointer_for_interruption() {
  local root="$1"
  local name="$2"
  local target="$3"
  local temporary="$root/.${name}.interruption-test"
  /bin/ln -s "$target" "$temporary"
  /bin/mv -fh "$temporary" "$root/$name"
}

release_one="$(make_release 0.1.0)"
release_two="$(make_release 0.2.0)"
if cmp -s "$workspace/0.1.0/grimmored" "$workspace/0.2.0/grimmored" || \
  cmp -s "$workspace/0.1.0/grimmore-launcher" "$workspace/0.2.0/grimmore-launcher"; then
  die "the upgrade smoke must stage distinct signed companion and launcher artifacts"
fi
install_root="$workspace/installed"
install_arguments=(
  --trusted-certificate-sha1 "$GRIMMORE_MACOS_TEST_CERT_SHA1"
  --trusted-team-id "$GRIMMORE_MACOS_TEST_TEAM_ID"
  --install-root "$install_root"
)

"$installer" --release "$release_one" "${install_arguments[@]}"
assert_pointer "$install_root" current 0.1.0
[[ ! -e "$install_root/previous" && ! -L "$install_root/previous" ]] || \
  die "first installation unexpectedly has a rollback pointer"
"$installer" --release "$release_two" "${install_arguments[@]}"
assert_pointer "$install_root" current 0.2.0
assert_pointer "$install_root" previous 0.1.0
"$installer" --release "$release_two" "${install_arguments[@]}"
assert_pointer "$install_root" current 0.2.0
assert_pointer "$install_root" previous 0.1.0

node "$repository/tests/release/installed-lifecycle.mjs" \
  --daemon "$install_root/bin/grimmored" \
  --launcher "$install_root/bin/grimmore-launcher" \
  --fixture-vault "$repository/tests/fixtures/vaults/reference-vault" \
  --workspace "$workspace/installed-lifecycle" \
  --endpoint "$runtime_directory/grimmore.sock"

# Simulate a power loss after the journaled rollback writes `previous` but
# before it replaces `current`. The next normal install must finish that
# switch before making any new decision, retaining one real rollback version.
printf 'schemaVersion=1\nfrom=versions/0.2.0\nto=versions/0.1.0\n' \
  >"$install_root/.pointer-switch"
/bin/chmod 600 "$install_root/.pointer-switch"
replace_pointer_for_interruption "$install_root" previous versions/0.2.0
"$installer" --release "$release_one" "${install_arguments[@]}"
assert_pointer "$install_root" current 0.1.0
assert_pointer "$install_root" previous 0.2.0
[[ ! -e "$install_root/.pointer-switch" && ! -L "$install_root/.pointer-switch" ]] || \
  die "interrupted pointer-switch journal was not recovered"
"$installer" --release "$release_two" "${install_arguments[@]}"
assert_pointer "$install_root" current 0.2.0
assert_pointer "$install_root" previous 0.1.0

tampered_directory="$workspace/tampered"
/bin/mkdir "$tampered_directory"
/usr/bin/ditto -x -k "$release_two" "$tampered_directory"
printf 'tamper' >>"$tampered_directory/grimmore-payload.zip"
tampered_release="$workspace/grimmore-release-0.2.1-$target.zip"
"$release_builder" \
  --envelope "$tampered_directory/Grimmore Release Envelope.app" \
  --payload "$tampered_directory/grimmore-payload.zip" \
  --out "$tampered_release" >&2
if "$installer" --release "$tampered_release" "${install_arguments[@]}"; then
  die "installer accepted a manifest-bound payload that was tampered after signing"
fi
assert_pointer "$install_root" current 0.2.0
assert_pointer "$install_root" previous 0.1.0

if "$installer" \
  --release "$release_two" \
  --trusted-certificate-sha1 0000000000000000000000000000000000000000 \
  --trusted-team-id "$GRIMMORE_MACOS_TEST_TEAM_ID" \
  --install-root "$install_root"; then
  die "installer accepted the release under an unpinned signing certificate"
fi
assert_pointer "$install_root" current 0.2.0
assert_pointer "$install_root" previous 0.1.0

"$install_root/bin/grimmored" doctor >"$workspace/doctor.json"
grep -Eq '"fts5Available"[[:space:]]*:[[:space:]]*true' "$workspace/doctor.json" || \
  die "installed doctor did not report bundled FTS5"
grep -Eq '"credentialStoreAvailable"[[:space:]]*:[[:space:]]*true' "$workspace/doctor.json" || \
  die "installed doctor did not complete a real macOS Keychain probe"
"$rollback" \
  --trusted-certificate-sha1 "$GRIMMORE_MACOS_TEST_CERT_SHA1" \
  --trusted-team-id "$GRIMMORE_MACOS_TEST_TEAM_ID" \
  --install-root "$install_root"
assert_pointer "$install_root" current 0.1.0
assert_pointer "$install_root" previous 0.2.0
printf 'macOS signed install, Keychain health, upgrade, tamper rejection, and rollback passed for %s\n' "$target"

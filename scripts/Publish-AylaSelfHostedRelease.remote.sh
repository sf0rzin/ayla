#!/bin/sh
set -eu

if [ "$#" -ne 5 ]; then
	printf 'usage: %s STAGE VERSION INSTALLER_SHA256 INSTALLER_SIZE NONCE\n' "$0" >&2
	exit 2
fi

stage=$1
version=$2
expected_hash=$3
expected_size=$4
nonce=$5

if ! awk -v value="$version" '
BEGIN {
  count = split(value, parts, "[.]")
  if (count != 3) exit 1
  for (index = 1; index <= 3; index++) {
    if (parts[index] !~ /^[0-9]+$/) exit 1
    if (length(parts[index]) > 1 && substr(parts[index], 1, 1) == "0") exit 1
  }
  exit 0
}' </dev/null; then
	printf 'invalid release version\n' >&2
	exit 2
fi
case "$nonce" in
	''|*[!0-9a-f]*) printf 'invalid publication nonce\n' >&2; exit 2 ;;
esac
case "$expected_hash" in
	''|*[!0-9a-f]*) printf 'invalid installer hash\n' >&2; exit 2 ;;
esac
if [ "${#expected_hash}" -ne 64 ]; then
	printf 'invalid installer hash\n' >&2
	exit 2
fi
case "$expected_size" in
	''|*[!0-9]*) printf 'invalid installer size\n' >&2; exit 2 ;;
esac

expected_stage="/tmp/ayla-release-$version-$nonce"
if [ "$stage" != "$expected_stage" ]; then
	printf 'unexpected staging path\n' >&2
	exit 2
fi

installer_name="Ayla_${version}_x64-setup.exe"
signature_name="$installer_name.sig"
installer="$stage/$installer_name"
signature="$stage/$signature_name"
metadata="$stage/latest.json"
publisher="$stage/Publish-AylaSelfHostedRelease.remote.sh"

updates_root=/srv/ayla-public/updates
release_root=$updates_root/releases
stable_root=$updates_root/stable
release_directory=$release_root/$version
candidate_directory=$release_root/.publish-$version-$nonce
metadata_temporary=$stable_root/.latest.$nonce.tmp
lock_path=$updates_root/.publish.lock

cleanup() {
	rm -rf -- "$candidate_directory" "$metadata_temporary" "$stage"
}
trap cleanup EXIT INT TERM HUP

for path in "$installer" "$signature" "$metadata" "$publisher"; do
	if [ ! -f "$path" ] || [ -L "$path" ]; then
		printf 'staged release contains an invalid file\n' >&2
		exit 1
	fi
done

actual_hash=$(sha256sum "$installer" | awk '{print $1}')
actual_size=$(stat -c %s "$installer")
if [ "$actual_hash" != "$expected_hash" ] || [ "$actual_size" != "$expected_size" ]; then
	printf 'staged installer failed size or SHA-256 verification\n' >&2
	exit 1
fi
if [ ! -s "$signature" ] || [ ! -s "$metadata" ]; then
	printf 'staged signature and metadata must not be empty\n' >&2
	exit 1
fi

version_is_greater() {
	awk -v requested="$1" -v current="$2" '
function valid(value, parts, count, index) {
  count = split(value, parts, "[.]")
  if (count != 3) return 0
  for (index = 1; index <= 3; index++) {
    if (parts[index] !~ /^[0-9]+$/) return 0
  }
  return 1
}
function compare_number(left, right) {
  sub(/^0+/, "", left); sub(/^0+/, "", right)
  if (left == "") left = "0"
  if (right == "") right = "0"
  if (length(left) != length(right)) return length(left) > length(right) ? 1 : -1
  if (left == right) return 0
  return ("x" left) > ("x" right) ? 1 : -1
}
BEGIN {
  if (!valid(requested, requested_parts) || !valid(current, current_parts)) exit 2
  for (component = 1; component <= 3; component++) {
    comparison = compare_number(requested_parts[component], current_parts[component])
    if (comparison > 0) exit 0
    if (comparison < 0) exit 1
  }
  exit 1
}' </dev/null
}

install -d -m 0750 -o root -g caddy "$updates_root" "$release_root" "$stable_root"
exec 9> "$lock_path"
flock -x 9

if [ -e "$stable_root/latest.json" ]; then
	if [ ! -f "$stable_root/latest.json" ] || [ -L "$stable_root/latest.json" ]; then
		printf 'published latest.json is not a regular file\n' >&2
		exit 1
	fi
	current_version=$(sed -n \
		's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)".*$/\1/p' \
		"$stable_root/latest.json")
	if [ -z "$current_version" ] || [ "$(printf '%s\n' "$current_version" | wc -l)" -ne 1 ]; then
		printf 'published latest.json has an invalid version\n' >&2
		exit 1
	fi
	if ! version_is_greater "$version" "$current_version"; then
		printf 'release version is not newer than the locked stable channel\n' >&2
		exit 1
	fi
fi

install -d -m 0750 -o root -g caddy "$candidate_directory"
install -m 0640 -o root -g caddy "$installer" "$candidate_directory/$installer_name"
install -m 0640 -o root -g caddy "$signature" "$candidate_directory/$signature_name"
install -m 0640 -o root -g caddy "$metadata" "$candidate_directory/latest.json"

metadata_source=$candidate_directory/latest.json
if [ -e "$release_directory" ]; then
	if [ ! -d "$release_directory" ] || [ -L "$release_directory" ] ||
		! cmp -s "$candidate_directory/$installer_name" "$release_directory/$installer_name" ||
		! cmp -s "$candidate_directory/$signature_name" "$release_directory/$signature_name" ||
		[ ! -f "$release_directory/latest.json" ] || [ -L "$release_directory/latest.json" ]; then
		printf 'existing immutable release differs from the staged release\n' >&2
		exit 1
	fi
	# A retry necessarily regenerates pub_date. Compare every other metadata field
	# and reuse the immutable first copy when the interrupted release is resumed.
	sed '/^[[:space:]]*"pub_date"[[:space:]]*:/d' \
		"$candidate_directory/latest.json" > "$candidate_directory/.candidate-metadata"
	sed '/^[[:space:]]*"pub_date"[[:space:]]*:/d' \
		"$release_directory/latest.json" > "$candidate_directory/.existing-metadata"
	if ! cmp -s "$candidate_directory/.candidate-metadata" "$candidate_directory/.existing-metadata"; then
		printf 'existing immutable release metadata differs from the staged release\n' >&2
		exit 1
	fi
	metadata_source=$release_directory/latest.json
	rm -rf -- "$candidate_directory"
else
	mv -- "$candidate_directory" "$release_directory"
	metadata_source=$release_directory/latest.json
fi

install -m 0640 -o root -g caddy "$metadata_source" "$metadata_temporary"
mv -f -- "$metadata_temporary" "$stable_root/latest.json"

trap - EXIT INT TERM HUP
rm -rf -- "$stage"

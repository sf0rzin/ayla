#!/bin/sh
set -eu

backup_directory=/var/backups/ayla
timestamp=$(date -u +%Y%m%dT%H%M%SZ)
temporary_path="$backup_directory/.ayla-$timestamp.dump.tmp"
backup_path="$backup_directory/ayla-$timestamp.dump"
temporary_checksum_path="$backup_directory/.ayla-$timestamp.dump.sha256.tmp"
checksum_path="$backup_path.sha256"
restore_database=""
restore_weekday=${AYLA_RESTORE_VERIFY_WEEKDAY:-7}
promotion_in_progress=false

install -d -m 0700 -o root -g root "$backup_directory"
umask 077
exec 9> "$backup_directory/.backup.lock"
if ! flock -n 9; then
	printf 'Another Ayla database backup is already running\n' >&2
	exit 1
fi
if [ -e "$backup_path" ] || [ -e "$checksum_path" ]; then
	printf 'Refusing to replace an existing timestamped backup\n' >&2
	exit 1
fi

compose() {
	docker compose \
		--env-file /opt/ayla/.env \
		-f /opt/ayla/compose.yaml \
		"$@"
}

cleanup() {
	if [ -n "$restore_database" ]; then
		compose exec -T postgres \
			dropdb --if-exists --force --username ayla "$restore_database" >/dev/null 2>&1 || true
	fi
	if [ "$promotion_in_progress" = true ]; then
		rm -f -- "$backup_path" "$checksum_path"
	fi
	rm -f -- "$temporary_path" "$temporary_checksum_path"
}
trap cleanup EXIT INT TERM

case "$restore_weekday" in
	[1-7]|never) ;;
	*) printf 'AYLA_RESTORE_VERIFY_WEEKDAY must be 1-7 or never\n' >&2; exit 2 ;;
esac

compose exec -T postgres \
	pg_dump --username ayla --dbname ayla --format custom > "$temporary_path"

# Unlike pg_restore --list, emitting the restore stream forces pg_restore to
# read and decompress every archived data block before the dump is promoted.
compose exec -T postgres \
	pg_restore --exit-on-error --file /dev/null < "$temporary_path"

backup_hash=$(sha256sum "$temporary_path" | awk '{print $1}')
case "$backup_hash" in
	''|*[!0-9a-f]*) printf 'Unable to compute the backup SHA-256\n' >&2; exit 1 ;;
esac
if [ "${#backup_hash}" -ne 64 ]; then
	printf 'Unable to compute the backup SHA-256\n' >&2
	exit 1
fi
printf '%s  %s\n' "$backup_hash" "$(basename "$backup_path")" > "$temporary_checksum_path"

# On the selected UTC weekday (Sunday by default), prove that the archive can
# populate an isolated database and that both application tables are readable.
if [ "$restore_weekday" != never ] && [ "$(date -u +%u)" = "$restore_weekday" ]; then
	restore_database="ayla_restore_$(printf '%s_%s' "$timestamp" "$$" | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9_')"
	compose exec -T postgres \
		createdb --username ayla --owner ayla --template template0 "$restore_database"
	compose exec -T postgres \
		pg_restore --exit-on-error --no-owner --no-privileges \
			--username ayla --dbname "$restore_database" < "$temporary_path"
	compose exec -T postgres \
		psql --username ayla --dbname "$restore_database" --no-align --tuples-only \
			--set ON_ERROR_STOP=1 \
			--command "SELECT count(*) FROM public.users; SELECT count(*) FROM public.sessions;" \
			> /dev/null
	compose exec -T postgres \
		dropdb --force --username ayla "$restore_database"
	restore_database=""
fi

promotion_in_progress=true
mv -- "$temporary_path" "$backup_path"
mv -- "$temporary_checksum_path" "$checksum_path"
(cd "$backup_directory" && sha256sum --check --status "$(basename "$checksum_path")")
promotion_in_progress=false
trap - EXIT INT TERM

find "$backup_directory" \
	-maxdepth 1 \
	-type f \
	\( -name 'ayla-*.dump' -o -name 'ayla-*.dump.sha256' \) \
	-mtime +14 \
	-delete

printf 'Created and verified %s (SHA-256 %s)\n' "$backup_path" "$backup_hash"

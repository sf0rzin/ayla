#!/bin/sh
set -eu

backup_directory=/var/backups/ayla
timestamp=$(date -u +%Y%m%dT%H%M%SZ)
temporary_path="$backup_directory/.ayla-$timestamp.dump.tmp"
backup_path="$backup_directory/ayla-$timestamp.dump"

install -d -m 0700 -o root -g root "$backup_directory"
umask 077

cleanup() {
	rm -f -- "$temporary_path"
}
trap cleanup EXIT INT TERM

docker compose \
	--env-file /opt/ayla/.env \
	-f /opt/ayla/compose.yaml \
	exec -T postgres \
	pg_dump --username ayla --dbname ayla --format custom > "$temporary_path"

docker compose \
	--env-file /opt/ayla/.env \
	-f /opt/ayla/compose.yaml \
	exec -T postgres \
	pg_restore --list < "$temporary_path" > /dev/null

mv -- "$temporary_path" "$backup_path"
trap - EXIT INT TERM

find "$backup_directory" \
	-maxdepth 1 \
	-type f \
	-name 'ayla-*.dump' \
	-mtime +14 \
	-delete

printf 'Created %s\n' "$backup_path"

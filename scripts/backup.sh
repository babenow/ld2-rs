#!/usr/bin/env bash
# Бэкап PostgreSQL с БД ld2 (PLAN-M12.8, M8-DoD: scripts/backup.sh).
#
# Использование:
#   scripts/backup.sh [каталог]           # дамп в <каталог>/ld2-<дата>-<время>.sql.gz
#   BACKUP_DIR=/var/backups/ld2 scripts/backup.sh
#
# Подключение (всё через переменные окружения, как и сервер):
#   PGHOST=127.0.0.1 PGPORT=5432 PGUSER=ld2 PGPASSWORD=ld2 PGDATABASE=ld2
# В docker compose:  docker compose exec -T postgres pg_dump -U ld2 ld2 | gzip > dump.sql.gz
#
# Восстановление (см. docs/deployment.md#восстановление-из-бэкапа):
#   gunzip -c ld2-….sql.gz | psql "$DATABASE_URL"        # поверх пустой БД
set -euo pipefail

BACKUP_DIR="${BACKUP_DIR:-./backups}"
PGHOST="${PGHOST:-127.0.0.1}"
PGPORT="${PGPORT:-5432}"
PGUSER="${PGUSER:-ld2}"
PGDATABASE="${PGDATABASE:-ld2}"
# PGPASSWORD — только из окружения (pgpass/.env); в скрипт не пишем.
# При локальном pg_dump пароль берётся из PGPASSWORD/PGPASSFILE.

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${BACKUP_DIR}/ld2-${STAMP}.sql.gz"

mkdir -p "${BACKUP_DIR}"

echo "бэкап ${PGDATABASE} @ ${PGHOST}:${PGPORT} → ${OUT}"
# pg_dump: локально, иначе — через контейнер postgres в docker compose
# (COMPOSE_SERVICE позволяет указать имя сервиса; "" — отключить fallback).
if command -v pg_dump >/dev/null 2>&1; then
  PGPASSWORD="${PGPASSWORD:-}" pg_dump \
    --host="${PGHOST}" --port="${PGPORT}" --username="${PGUSER}" \
    --dbname="${PGDATABASE}" \
    --no-owner --no-privileges --clean --if-exists \
    | gzip -9 > "${OUT}"
elif command -v docker >/dev/null 2>&1 && [ "${COMPOSE_SERVICE:-postgres}" != "" ]; then
  docker compose exec -T "${COMPOSE_SERVICE:-postgres}" \
    pg_dump --username="${PGUSER}" --dbname="${PGDATABASE}" \
    --no-owner --no-privileges --clean --if-exists \
    | gzip -9 > "${OUT}"
else
  echo "ошибка: pg_dump не найден (и docker compose fallback отключён)" >&2
  exit 1
fi

# Целостность: gzip -t и непустой файл.
gzip -t "${OUT}"
SIZE="$(wc -c < "${OUT}")"
if [ "${SIZE}" -lt 64 ]; then
  echo "ошибка: дамп подозрительно мал (${SIZE} байт)" >&2
  rm -f "${OUT}"
  exit 1
fi

# Ретенция: храним последние 14 дампов (RETENTION=0 — без чистки).
RETENTION="${RETENTION:-14}"
if [ "${RETENTION}" -gt 0 ] 2>/dev/null; then
  ls -1t "${BACKUP_DIR}"/ld2-*.sql.gz 2>/dev/null \
    | tail -n +$((RETENTION + 1)) \
    | while read -r old; do
        echo "ретенция: удаляю ${old}"
        rm -f "${old}"
      done
fi

echo "готово: ${OUT} ($(du -h "${OUT}" | cut -f1))"

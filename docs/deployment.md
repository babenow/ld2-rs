# Развёртывание ld2-server

Эксплуатационный гид (PLAN-M12.8): установка, reverse-proxy с `wss`,
миграции, бэкап/восстановление, config-reference всех ключей `server.toml`.

Контракт: [`protocol.md`](./protocol.md) (§1 — `wss://` обязателен вне
localhost, сабпротокол `ld2.v1`). Все настройки сервера — **только** в
`server.toml` (D25); переменные окружения нужны лишь для путей/паролей БД.

---

## 1. Быстрый старт (docker compose)

```bash
cp crates/ld2-server/server.example.toml server.toml   # и правки: database.url
docker compose up --build -d
curl http://127.0.0.1:24817/healthz                    # → 200 ok
```

`docker compose` поднимает PostgreSQL 16 (volume `pgdata`) и сервер
(`Dockerfile`, Alpine/musl). Контент мира монтируется в `/content`,
конфиг — в `/etc/ld2/server.toml`.

## 2. Без Docker (systemd)

```bash
cargo build --release -p ld2-server
sudo install -m 755 target/release/ld2-server /usr/local/bin/ld2-server
sudo install -m 600 server.toml /etc/ld2/server.toml   # пароль БД в файле!
sudo install -m 755 -d /var/lib/ld2                    # рабочий каталог
```

`/etc/ld2/server.toml` — **`chmod 600`**, владелец — пользователь сервиса:
файл содержит `database.url` с паролем.

`/etc/systemd/system/ld2-server.service`:

```ini
[Unit]
Description=ld2 game server
After=network.target postgresql.service

[Service]
Type=simple
User=ld2
WorkingDirectory=/var/lib/ld2
ExecStart=/usr/local/bin/ld2-server --config /etc/ld2/server.toml run
ExecStartPost=/usr/local/bin/ld2-server --config /etc/ld2/server.toml migrate
Restart=on-failure
# SIGTERM → graceful shutdown (дождаться запросов и флаша, §M12.1)
KillSignal=SIGTERM
TimeoutStopSec=15

[Install]
WantedBy=multi-user.target
```

Миграции применяются **до** старта приёма (`migrate` отдельной командой
или автоматически при `run` — `run` тоже применяет, fail-fast до
слушателя). Откат одной версии: `ld2-server --config … revert`.

## 3. Reverse-proxy: `wss://` терминал

Клиент в release-сборке принимает **только `wss://`** (localhost —
исключение). Терминальным хостом WS всегда является proxy; сервер слушает
`http://127.0.0.1:24817` (не публикуйте порт наружу).

`nginx`:

```nginx
server {
    listen 443 ssl;
    server_name play.example.ru;
    ssl_certificate     /etc/letsencrypt/live/play.example.ru/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/play.example.ru/privkey.pem;

    location /ws {
        proxy_pass http://127.0.0.1:24817;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 120s;   # ≥ тишины между пингами клиента
        # §1: лимит фрейма клиента — 64 KiB; держим и на proxy.
        client_max_body_size 64k;
    }

    location /healthz {
        proxy_pass http://127.0.0.1:24817;
    }
}
```

`caddy` (минимум конфига):

```caddyfile
play.example.ru {
    reverse_proxy /ws* 127.0.0.1:24817
    reverse_proxy /healthz 127.0.0.1:24817
}
```

Что **не** нужно настраивать на proxy: балансировку внутри одной сессии
(WS-соединение привязано к процессу, D31 — один шард) и буферизацию ответов
(`proxy_buffering off` для `/ws`, если включено по умолчанию).

## 4. Обновление

```bash
docker compose build server && docker compose up -d server   # или
sudo systemctl stop ld2-server
sudo install -m 755 target/release/ld2-server /usr/local/bin/ld2-server
sudo systemctl start ld2-server
```

Порядок: **бэкап → остановка → бинарь → миграции → старт**. Миграции
идемпотентны; `run` применит недостающие до открытия порта (fail-fast).
Контент-сид (D26) идемпотентен: неизменённый `content/` не трогает БД,
изменённый — апсертит по id (ручные правки в БД переживают рестарт).

## 5. Бэкап и восстановление

### Бэкап

```bash
# локально/на хосте:
PGPASSWORD=… ./scripts/backup.sh /var/backups/ld2

# в docker compose:
docker compose exec -T postgres pg_dump -U ld2 ld2 | gzip > ld2-$(date -u +%s).sql.gz
```

`scripts/backup.sh`: `pg_dump --clean --if-exists` → gzip (с проверкой
целостности), ретенция последних 14 дампов (`RETENTION`).

### Восстановление

1. Остановить сервер (иначе схема может разойтись с дампом):
   `systemctl stop ld2-server` / `docker compose stop server`.
2. Восстановить БД на **пустую** базу:

```bash
gunzip -c backups/ld2-20261106T120000Z.sql.gz | psql "$DATABASE_URL"
# или в docker:
gunzip -c dump.sql.gz | docker compose exec -T postgres psql -U ld2 ld2
```

3. Привести схему к последней версии (дамп может быть старее миграций):
   `ld2-server --config … migrate`.
4. Запустить сервер и проверить: `curl /healthz` → `200 ok`, лог старта
   содержит «миграции применены»/«контент-сид».

Проверка после восстановления: вход клиентом → `state.resync` → снапшот
(инвентарь/навыки/квесты на месте); приёмка — прогон `cargo test`
против восстановленной БД (`LD2_TEST_DATABASE_URL`).

## 6. Мониторинг и диагностика

- `/healthz` → `200 ok` при живой БД, `503` иначе (для чекеров).
- Логи: `tracing` в stdout, уровень — `server.log_level`; в systemd → journald.
- Норма в логе: пусто или редкие `WARN` (`last_char_id` у свежих БД —
  безобидно, колонка ещё не заполнена).
- Типовые ошибки: `E_RATE_LIMITED` (кулдауны, `retry_after_s`),
  `E_STATION_REQUIRED` (крафт вне станка), `E_TRADE_STALE` (окно сделки).

## 7. Config-reference: все ключи `server.toml`

Формат: `ключ — тип — по умолчанию/пример — что делает`. Значения берутся
**только** из файла (D25); невалидное значение → fail-fast с указанием
ключа при старте.

### `[server]`

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `bind` | строка | `0.0.0.0:24817` | Адрес WS/HTTP-слушателя (за reverse-proxy) |
| `name` | строка | `ld2-dev` | Имя сервера в `evt/session.start.server` |
| `region` | строка | `ru` | Регион в `session.start.server.region` |
| `log_level` | строка | `info` | Уровень `tracing` (`error`…`debug`); невалидный → ошибка запуска |
| `min_client` | строка? | `null` | `force-min-version` capability (§12); `null` — без требований |

### `[database]`

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `url` | строка | `postgres://…` | Подключение PostgreSQL (пароль **в файле**, chmod 600) |
| `max_connections` | u32 | `10` | Пул sqlx |

### `[auth]`

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `access_ttl_s` | u64 | `900` | TTL access-токена (opaque, sha256 в БД — D29) |
| `refresh_ttl_s` | u64 | `259200` | TTL refresh-токена; должен быть > `access_ttl_s` |
| `min_login_len` | u32 | `3` | Минимум логина → `E_LOGIN_TAKEN`/валидация |
| `min_password_len` | u32 | `6` | Минимум пароля → `E_WEAK_PASSWORD` |
| `register_enabled` | bool | `true` | Выключено → `E_REGISTER_DISABLED` |

Rate-limit auth-команд по IP (D30) — фиксированное значение в коде
(`auth::AuthRateLimiter`, не конфиг): превышение → `E_RATE_LIMITED` +
`retry_after_s`.

### `[world]`

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `content_dir` | путь | `content` | Каталог JSON-контента (D26) |
| `tick_hz` | u32 | `10` | Частота тик-лупа, 1..=100 (D31): бой, реген, джобы |
| `autosave_interval_s` | u64 | `30` | Периодический флаш (D27) |

### `[gameplay]`

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `char_slot_limit` | u32 | `8` | Лимит слотов персонажей (§5) |
| `profession_limit` | u32 | `2` | Лимит **первичных** профессий (§5) |
| `party_max_members` | u32 | `4` | Лимит состава группы (ФР-14) → `E_PARTY_FULL` |

### `[gameplay.combat]` (ФР-05, M12.5)

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `attack_interval_s` | f64 | `1.0` | Период атак в бою (тик-луп) |
| `base_damage` | i64 | `6` | Урон персонажа (v1 без формул оружия) |
| `flee_chance` | f64 | `0.5` | Шанс `cmd/combat.flee`, `0..=1`; провал → `E_COOLDOWN` |

### `[gameplay.features]` (capability-флаги §5, ключи — в кавычках)

`"loot.roll"` (bool), `"respawn.inn"` (bool), `"guild.bank"` (bool),
`"upgrade.rules"` (строка: `keep`/`level_down`/`break_on_fail`),
`"trade.value_hint"` (bool), `"chat.slash"` (bool).
Неизвестные ключи разрешены (фичи открыты).

### `[gameplay.crime]` (§10)

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `t_max_s` | u64 | `604800` | Потолок суммарного срока `T_MAX` |
| `blend_k` | f64 | `0.5` | Коэффициент частичного сложения `k`, `(0;1]` |
| `[gameplay.crime.base]` | u64×N | `murderer_s=21600` | Базовые сроки по видам (суффикс `_s`) |
| `[[gameplay.crime.kinds]]` | таблицы | `kind/title/severity` | Активные статусы (`low`/`medium`/`high`) |

### `[gameplay.chat]` (ФР-13)

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `server_history` | u32 | `200` | Кольцо истории в БД (0 — не хранить/не реплеить) |
| `max_len` | u32 | `500` | Длина текста → `E_RATE_LIMITED` |
| `cooldown_s` | f64 | `1.5` | Кулдаун сообщения → `E_RATE_LIMITED` + `retry_after_s` |
| `channels` | массив | `[...]` | Разрешённые каналы; дубликаты/пустоты — ошибка конфига |

### `[gameplay.inventory]`

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `slots` | u32 | `40` | Слоты инвентаря → `E_INVENTORY_FULL` |
| `equip_slots` | массив | `["head",…]` | Слоты экипировки (ключи §6 `equip`) |

### `[gameplay.upgrade]` (ФР-10)

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `max_level` | u32 | `9` | Потолок `+N` → `E_UPGRADE_MAX` |
| `preview` | bool | `true` | Превью включено (capability) |
| `shows_chance` | bool | `true` | Отдавать ли `chance` в превью |
| `gold_base` / `gold_per_level` | i64 | `100` / `50` | Стоимость: `base + per×current` |
| `chance_base` / `chance_per_level` | f64 | `0.9` / `0.05` | Шанс: `base − per×current`, кламп `0..=1` |

### `[gameplay.magic]` (ФР-09, D4/ADR-002)

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `rune_recharge` | строка | `item` | Режим перезарядки рун |
| `scroll_requires_target` | bool | `true` | Свитку нужна цель → `E_TARGET_GONE` |
| `mana.regen_out_s` | u64 | `45` | Полный бар маны вне боя, с |
| `mana.regen_in_pct_s` | f64 | `1.0` | Реген в бою, %/с |
| `mana.rune_cost` | u32 | `10` | Стоимость каста руны → `E_NOT_ENOUGH_MP` |

### `[skills]`

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `base_xp` / `growth` | i64 / f64 | `120` / `1.32` | Порог ранга: `round(base×growth^rank)` |
| `rank_max` | u32 | `30` | Потолок ранга → `E_RANK_CAP` |
| `gates` | таблица rank→attr | `{ 10 = 12 }` | Гейты атрибутов → `E_REQUIREMENT_ATTR` |
| `kill_xp_base` / `kill_xp_per_difficulty` | i64 | `8` / `6` | XP убийства: `base + difficulty×per` |

### `[gameplay.professions]` (skills.md §8, M12.6)

| Ключ | Тип | Пример | Назначение |
|---|---|---|---|
| `rank_max` | u32 | `7` | Предел ранга профессии |
| `xp_per_craft_base` / `xp_per_craft_per_rank` | i64 | `40` / `25` | XP за крафт: `base + per×ранг_рецепта` |
| `xp_to_next_base` / `xp_growth` | i64 / f64 | `80` / `1.5` | Порог ранга: `round(base×growth^rank)` |
| `fail_xp_ratio` | f64 | `0.2` | Доля XP при неудаче |

---

Связанные документы: [`protocol.md`](./protocol.md) — контракт и §13
(чек-лист приёмки), [`tz.md`](./tz.md) §8 — НФР, [`user-manual.md`](./user-manual.md)
— руководство игрока, `docker-compose.yml` — референс прода.

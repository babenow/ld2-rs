# Протокол ld2 — контракт «клиент ↔ сервер»

**Версия контракта (wire):** `v = 1` · **Статус:** нормативный (экспорт и развёртывание §4 [`tz.md`](./tz.md))
**Контракт обязателен для трёх сторон:** `ld2-client`, `ld2-mock-server`, будущего реального сервера.
**Единственный источник типов:** crate `ld2-protocol` (serde-типы без зависимостей от UI/сети).

Соглашения этого документа: ключи JSON — `snake_case`; UTF-8; «MUST/SHOULD» = «ОБЯЗАТЕЛЬНО/РЕКОМЕНДУЕТСЯ».

---

## 1. Транспорт

| Параметр | Значение |
|---|---|
| Протокол | WebSocket (`wss://` в release, `ws://` только dev/localhost) |
| Фреймы | текстовые, ровно один JSON-объект на сообщение; бинарные фреймы = ошибка протокола, соединение закрывается |
| Макс. размер | 64 KiB (на сообщение). Больше — закрытие `E_TOO_LARGE`; крупные данные (снапшоты) нарезаются |
| Keepalive | WS ping/pong (сервер: 30 с, ожидание pong 10 с) + прикладной `cmd/ping → evt/pong` |
| Рекомендуемый заголовок | `Sec-WebSocket-Protocol: ld2.v1` (негативный ответ сервера не фатален для dev) |
| Кодировки | номера — целые; суммы в золоте — целые (никогда float); вероятности — 0.0…1.0 (float, только если сервер их раскрывает) |

### 1.1 Время (КРИТИЧНО)

- **`ts` в конверте** — строка ISO-8601 в UTC (`"2026-11-02T21:05:11Z"`) для логов и отладки.
- **Все игровые дедлайны** (сроки статусов, cooldown'ы, таймеры сделок, `expires_at`) — **целые epoch-секунды UTC**.
- Клиент хранит `time_offset = median(5 × (server_ts − local_now))`, перекалибровка раз в 5 мин;
  локальный обратный отсчёт = `expires_at − (now + offset)`. **Сервер — истина по времени.**
- Клиенты с системными часами в прошлом/будущем не должны ломать работу UI.

---

## 2. Конверт сообщений

### 2.1 Команда (клиент → сервер)

```json
{
  "v": 1,
  "type": "cmd/location.move",
  "seq": 42,
  "ts": "2026-11-02T21:05:11Z",
  "payload": { "exit_id": "e_north" }
}
```

| Поле | Обяз. | Описание |
|---|---|---|
| `v` | да | wire-версия протокола (см. §11) |
| `type` | да | `cmd/<domain>.<action>` |
| `seq` | да | u32, монотонно возрастающий, **обрывается** (wrapping); старт — случайный |
| `ts` | да | ISO-8601 UTC |
| `payload` | нет | схема по домену (§7); пустая команда → `{}` или поле опущено |

### 2.2 Подтверждение (сервер → клиент)

```json
{ "v": 1, "type": "ack", "seq": 42, "state_version": 1337, "result": "ok",
  "payload": { "location": {} } }

{ "v": 1, "type": "ack", "seq": 42, "state_version": 1337, "result": "error",
  "error": { "code": "E_EXIT_BLOCKED", "message": "Выход завален", "retry_after_s": null } }
```

| Поле | Обяз. | Описание |
|---|---|---|
| `seq` | да | эхо seq команды |
| `result` | да | `"ok"` \| `"error"` |
| `state_version` | да* | версия состояния персонажа ПОСЛЕ команды (см. §3); `null` — если состояние не менялось (*да для игровых команд) |
| `payload` | нет | результат команды (например, новая локация) — НЕ через отдельные события, чтобы не было гонки |
| `error` | при `error` | `{ code, message, retry_after_s? }` |

Гарантия: **на каждую команду ровно один ack** (или клиентская таймаут-обработка, §2.4).
Все события, обусловленные командой, отправляются **после** её ack.

### 2.3 Событие (сервер → клиент)

```json
{ "v": 1, "type": "evt/chat.message", "ts": "2026-11-02T21:05:12Z",
  "state_version": 1337,
  "payload": { "channel": "location", "from": "p_77", "name": "Марк", "text": "кто в пати?" } }
```

- `state_version` (необяз.) — версия состояния, которой соответствует событие;
  клиент МУСТ игнорировать события со `state_version` < уже применённой (защита от гонок/повторов).
- Неизвестный `type` или `v > 1` → клиент: логирует `tracing::warn`, **игнорирует**, не паникует (кроме §11 для `v >`).

### 2.4 Правила надёжности

1. **Один ack на команду.** Таймаут клиента — 10 с. По таймауту клиент может повторить команду **только если** она идемпотентна (§7), не более 1 раза.
2. **Идемпотентность на сервере:** окно последних 128 `seq`-ов сессии; повтор уже обработанного `seq` → вернуть **точный кэшированный ack**, не выполняя действие повторно.
3. **Порядок:** в рамках одного WS-соединения события доставляются в порядке отправки сервером; клиент МУСТ применять их последовательно (не переупорядочивать).
4. **При потере** (разрыв) — только полное восстановление через §3 (snapshot/delta), никаких догадок.
5. **Гонка ack vs push:** события не «обгоняют» свои ack (см. §2.2 последний абзац), но клиент обязан уметь пережить push-событие при ещё не закрытом ack.

---

## 3. Модель состояния: snapshot + delta + события

- Единственный источник истины — **сервер**. Клиент **не считает** исходы (урон, шансы, сроки) — только рендерит данные.
- Состояние персонажа снабжено монотонным `state_version` (u64 на сессию, НЕ сбрасывается при смене локации).
- На `cmd/char.select` (и всегда при успешном `auth.resume`) сервер шлёт `evt/state/snapshot` — полное состояние:
  персонаж, инвентарь/экипировка, квесты, статусы, party, guild, текущая локация + видимые сущности.
- Далее — только `evt/…` события и `ack` с `state_version`.
- `cmd/state.resync { last_state_version }` → сервер отвечает либо `evt/state/delta` (может достроить), либо полным
  `evt/state/snapshot`. Клиент МУСТ поддерживать **полную замену сторов** в любой момент.

```json
// evt/state/delta — минимальный JSON-Patch-like subset (свой формат, не полная RFC 6902)
{ "v": 1, "type": "evt/state/delta", "ts": "…", "state_version": 1340, "payload": {
    "from_version": 1337, "to_version": 1340,
    "patch": [
      { "op": "replace", "path": "/char/hp/cur", "value": 31 },
      { "op": "remove",  "path": "/location/entities/m_3" },
      { "op": "add",     "path": "/location/entities/p_99", "value": { "id": "p_99", "kind": "player", "name": "Сара" } }
    ] } }
```

Правила patch: `path` — JSON Pointer от корня снапшота; `op` ∈ {`add`, `remove`, `replace`}; отсутствие пути
при `remove/replace` — **ошибка клиентской апликации**: клиент МУСТ сброситься на `state.resync`.

---

## 4. Сессия и авторизация

```
connect
  → cmd/auth.register { login, password, email }    → ack {payload: access_token, expires_in_s}
                                                  ← evt/session.start (регистрация = немедленный вход)
  → cmd/auth.login { login, password }            → ack {payload: access_token, expires_in_s}
  ← evt/session.start { config, characters[], refresh_token {token, expires_in_s} }
  → cmd/char.select { char_id }                   → ack
  ← evt/state/snapshot
  … игра …
```

| Команда/событие | Назначение |
|---|---|
| `cmd/auth.register` `{login, password, email}` | регистрация и **немедленный вход** (ack + `session.start`, как `login`); **email обязателен, нормализуется (`trim` + lowercase) и уникален** (D33, ADR-007); лимиты длины — настройки сервера; пароль только по TLS/WSS |
| `cmd/auth.login` `{login, password}` | первый вход; пароль уходит только по TLS/WSS |
| `cmd/auth.resume` `{refresh_token}` | восстановление сессии при reconnect (вместо login) |
| `cmd/session.refresh` | → ack с новым `access_token` (клиент трекает `expires_in_s − 60 с`) |
| `cmd/auth.logout` | отзыв refresh-токена, закрытие сессии |
| `evt/session.start` | см. §6; повторяется и после resume |
| `ack` с `error.code = E_AUTH_EXPIRED` | клиент → экран входа, UI-настройки сохраняются |

**Хранение токенов клиентом:** только ОС-хранилище (keyring: macOS Keychain / Windows Credential
Manager / Secret Service), service = `ld2.client`, ключ = `<host>:<login>`. Никаких токенов в конфигах/логах.

---

## 5. Конфиг аккаунта и capabilities (`evt/session.start`)

Клиент **обязан** брать все лимиты/фичи отсюда, без хардкода; незнакомые ключи — игнорировать.

```json
{
  "server": { "name": "ld2-dev", "region": "ru", "min_client": null },
  "session": { "access_token": "…", "expires_in_s": 900,
               "refresh": { "token": "…", "expires_in_s": 2592000 } },
  "char_slot_limit": 8,
  "profession_limit": 2,
  "features": {
    "loot.roll": false, "respawn.inn": true, "guild.bank": false,
    "upgrade.rules": "break_on_fail", "trade.value_hint": true, "chat.slash": true
  },
  "crime": {
    "t_max_s": 604800, "blend_k": 0.5,
    "base": { "murderer_s": 21600, "marauder_s": 7200 },
    "kinds": [
      { "kind": "murderer", "title": "Убийца", "severity": "high" },
      { "kind": "marauder", "title": "Мародёр", "severity": "medium" },
      { "kind": "thief", "title": "Вор", "severity": "medium" }
    ]
  },
  "chat": { "server_history": 200,
            "channels": ["system","location","global","party","guild","whisper"],
            "max_len": 500, "cooldown_s": 1.5 },
  "inventory": { "slots": 40, "equip_slots": ["head","body","main_hand","staff","feet"] },
  "upgrade": { "max_level": 9, "preview": true, "shows_chance": true },
  "magic": { "rune_recharge": "item", "scroll_requires_target": true,
             "mana": { "regen_out_s": 45, "regen_in_pct_s": 1.0, "rune_cost": 10 } },
  "characters": [
    { "id": "c1", "name": "Ким", "profile": "warrior", "location": "Черный лес",
      "summary": "Навыки: меч 3, аркана 1", "crime": ["murderer"], "online": true }
  ]
}
```

`profile` в `characters[]` — опционально (аддитивно, ADR-005): класс из
`char.create` (`warrior`/`rogue`/`mage`) для цвета ника на экране выбора;
сервер без поля валиден — клиент рисует нейтрально.

- `kinds` — расширяемый список видов статусов; клиент рендерит любые `kind` через title/severity, незнакомые — generic.
- `min_client` (string|null) — если не `null` и версия приложения меньше → экран «Требуется обновление», вход блокируется.

---

## 6. Схемы общих типов

> Дополнительные состояния контракта — `CharacterState`, `PartyState`, `GuildState`, `TradeState`,
> `GuildLogEntry`, `CrimeLogEntry` — нормализованы в `ld2-protocol` (`types.rs`/`evt.rs`) и сверяются
> golden-тестами (`crates/ld2-protocol/tests/golden.rs`). Поля с `…` в примерах и открытые строки
> (`kind`, `rarity`, `severity`, `channel`) сервер может расширять — клиент обязан съедать (§11).

```jsonc
// Hp
{ "cur": 42, "max": 60 }

// Mp — ресурс заклинаний (D4): каст руны тратит ману и заряд; свиток — без маны
{ "cur": 45, "max": 60 }

// Entity (строка в location.entities)
{ "id": "p_77", "kind": "player",           // player | monster | npc | corpse | object
  "name": "Марк",
  "hp": { "cur": 30, "max": 55 },           // у npc/corpse может отсутствовать
  "tags": ["marauder"],                     // любые игровые теги-статусы
  "was_player": true,                       // только для corpse — труп игрока (!!)
  "aggro": false,
  "…": "…" }                                // расширимые поля (extra) сохраняются, §11.2

// Exit
{ "id": "e_north", "label": "тропа на север", "dir": "north" }   // dir — опционально

// LocationSnapshot (в снапшоте или в ack payload)
{ "id": "l_forest", "name": "Черный лес", "flags": ["pvp"], "text": "Сосны давят тьмою…",
  "exits": [ Exit ], "entities": [ Entity ] }

// ItemSnapshot
{ "uid": "i_17", "type": "staff",            // weapon|armor|staff|potion|scroll|rune|material|quest
  "name": "Ясеневый посох", "rarity": "rare",
  "upgrade": 2,                              // +N, 0 если не улучшен
  "stats": { "dmg": "3..5", "magic": 2 },
  "requires": { "attr": { "int": 12 }, "profession": null, "level": null },
  "sockets": [ { "index": 0, "rune": { "name": "Руна огня", "spell": "fire_dart", "charges": 7, "max": 10 } },
               { "index": 1, "rune": null } ],
  "qty": 1, "gold_value": 120 }

// SkillSnapshot
{ "id": "sword", "name": "Владение мечом", "rank": 3, "xp": 612, "xp_to_next": 480 }

// CrimeStatus
{ "kind": "murderer", "severity": "high", "expires_at": 1762150000 }

// QuestState
{ "id": "q_boars", "title": "Кабаны", "state": "active",     // available|active|turn_in|done
  "goals": [ { "text": "Кабанов убито", "cur": 2, "need": 5 } ],
  "reward_preview": { "xp": 450, "items": [] } }
```

---

## 7. Команды (`cmd/*`)

Условные обозначения в графе «Идемп.»: **+** — повтор безопасен и возвращает кэшированный ack, **−** — повтор запрещён (клиент не должен ретраить).

| Команда | Payload | Ack payload | Идемп. | Назначение |
|---|---|---|---|---|
| `auth.login` | `{login, password}` | `{access_token, expires_in_s}` | + | вход |
| `auth.register` | `{login, password, email}` | `{access_token, expires_in_s}` | + | регистрация и немедленный вход (→ `evt/session.start`); лимиты — настройки сервера; `email` обязателен и уникален (`E_BAD_EMAIL`/`E_EMAIL_TAKEN`) |
| `auth.resume` | `{refresh_token}` | `{access_token, expires_in_s}` | + | reconnect |
| `auth.logout` | `{}` | `{}` | + | выход |
| `session.refresh` | `{}` | `{access_token, expires_in_s}` | + | ротация токена |
| `char.list` | `{}` | `{characters[]}` | + | список персонажей |
| `char.create` | `{name, profile}` | `{character}` | − | создание (profile: Воин/Вор/Маг, список — capability сервера) |
| `char.select` | `{char_id}` | `{}` (→ `evt/state/snapshot`) | + | вход в мир |
| `char.delete` | `{char_id}` | `{}` | − | удаление |
| `char.xp_invest` | `{skill_id, points}` | `{pool_left}` | − | вложить очки опыта в навык |
| `location.look` | `{}` | `{location}` | + | перерисовать локацию |
| `location.move` | `{exit_id}` | `{location}` | − | переход |
| `entity.info` | `{entity_id}` | `{entity_detail}` | + | осмотр сущности/трупа |
| `combat.attack` | `{target_id}` | `{}` | − | удар |
| `combat.stop` | `{}` | `{}` | + | выйти из боя |
| `combat.flee` | `{}` | `{}` | − | попытка сбежать |
| `skill.use` | `{skill_id, target_id?}` | `{}` | − | активный навык |
| `item.use` | `{item_uid, target_id?}` | `{}` | − | зелье/свиток-прочее |
| `item.equip` | `{item_uid, slot}` | `{char, item}` | + | надеть |
| `item.unequip` | `{slot}` | `{char, item}` | + | снять |
| `item.drop` | `{item_uid, qty?}` | `{}` | − | выбросить (в PvP-зоне = лут) |
| `magic.scroll_cast` | `{item_uid, target_id?}` | `{}` | − | каст со свитка |
| `magic.rune_cast` | `{staff_uid, socket_index, target_id?}` | `{}` | − | каст с руны посоха |
| `socket.insert` | `{staff_uid, socket_index, rune_uid}` | `{staff}` | − | инкрустация |
| `socket.extract` | `{staff_uid, socket_index}` | `{staff, rune}` | − | извлечение |
| `upgrade.preview` | `{item_uid}` | `{preview_id, delta{}, cost{}, chance?}` | + | превью «сейчас→после» |
| `upgrade.start` | `{item_uid, preview_id}` | `{}` (→ `evt/upgrade/result`) | − | улучшение |
| `craft.list` | `{}` | `{professions[]}` | + | мои профессии: id/ранг/xp |
| `craft.recipes` | `{profession?}` | `{recipes[]}` | + | список рецептов |
| `craft.learn` | `{profession_id}` | `{profession}` | − | изучить профессию у NPC |
| `craft.start` | `{recipe_id, qty}` | `{}` (→ `evt/craft/result`) | − | крафт |
| `quest.list` | `{}` | `{quests[]}` | + | журнал квестов |
| `quest.accept` / `quest.turn_in` / `quest.abandon` | `{quest_id}` | `{quest}` | − | взаимодействие |
| `crime.log` | `{limit?}` | `{entries[]}` | + | журнал нарушений |
| `chat.send` | `{channel, text, to?}` | `{}` (→ `evt/chat.message`) | − | сообщение |
| `party.create` | `{}` | `{party}` | − | создать группу |
| `party.invite` | `{player_id}` | `{}` (→ `evt/party/updated`) | − | пригласить |
| `party.accept` / `party.decline` | `{invite_id}` | `{party}` | − | ответить |
| `party.leave` | `{}` | `{}` | + | выйти |
| `party.kick` | `{player_id}` | `{}` | − | исключить |
| `trade.open` | `{player_id}` | `{offer_id}` (→ `evt/trade/state`) | − | открыть сделку |
| `trade.add_item` | `{offer_id, item_uid, qty?}` | `{}` (→ `evt/trade/state`) | − | положить предмет |
| `trade.set_gold` | `{offer_id, gold}` | `{}` (→ `evt/trade/state`) | − | предложить золото |
| `trade.propose` / `trade.accept` | `{offer_id, version}` | `{}` | − | зафиксировать (сверка version) |
| `trade.cancel` | `{offer_id}` | `{}` | + | отмена |
| `guild.browse` | `{query?}` | `{guilds[]}` | + | поиск гильдий |
| `guild.create` | `{name}` | `{guild}` | − | создать (по правилу сервера) |
| `guild.join` / `guild.leave` | `{guild_id?}` | `{}` | − | вступить/выйти |
| `guild.invite` / `guild.kick` | `{player_id}` | `{}` | − | пригласить/исключить |
| `guild.rank_set` | `{player_id, rank}` | `{}` | + (атомарно) | смена ранга |
| `state.resync` | `{last_state_version}` | `{}` (→ snapshot/delta) | + | восстановление |
| `ping` | `{}` | `{}` (→ `evt/pong`) | + | RTT |
| `time.sync` | `{}` | `{}` (→ `evt/time.sync`) | + | калибровка времени |

**Ограничения текста чата:** `chat.max_len`, `chat.cooldown_s` из §5; превышение → `E_RATE_LIMITED` c `retry_after_s`.

---

## 8. События (`evt/*`)

### Сессия/состояние
| Событие | Payload | Назначение |
|---|---|---|
| `evt/session.start` | §5 | конфиг + токены + список персонажей |
| `evt/state/snapshot` | `{ state_version, char, location, party, guild, server_ts }` | полное состояние |
| `evt/state/delta` | §3 | патч состояния |
| `evt/pong` | `{ ts, rtt_ms }` | эхо пинга |
| `evt/time.sync` | `{ server_ts, rtt_hint_ms? }` | калибровка offset |
| `evt/error` | `{ code, message, fatal? }` | серверные уведомления вне ack (не-фатальные) |

### Персонаж и мир
| Событие | Payload | Назначение |
|---|---|---|
| `evt/char/xp` | `{ skill_id, amount, skill_xp, rank?, xp_to_next?, pool_left? }` | начисление XP (`rank` — при повышении, `pool_left` — остаток XP-пула после вложения) |
| `evt/char/stats` | `{ patch }` | изменение атрибутов/HP/MP |
| `evt/char/death` | `{ victim_char: true, lost_items[], corpse, respawn { location_id, delay_s } }` | смерть ПЕРСОНАЖА, полная потеря лута |
| `evt/location/changed` | `{ location: LocationSnapshot }` | новая локация (broadcast в ней же) |
| `evt/entity/spawn` | `{ entity: Entity, location_id }` | появление сущности |
| `evt/entity/move` | `{ entity_id, patch }` | перемещение/изменение внутри локации |
| `evt/entity/hp` | `{ entity_id, hp }` | смена HP |
| `evt/entity/despawn` | `{ entity_id, location_id }` | исчезновение (смерть/уход) |
| `evt/corpse/created` | `{ entity: Entity, loot_count }` | труп (`was_player` различает игрока) |
| `evt/corpse/looted` | `{ corpse_id, by, by_name, was_player }` | лут собран; если `was_player` — клиент показывает предупреждение о статусе |

### Бой и магия
| Событие | Payload | Назначение |
|---|---|---|
| `evt/combat/hit` | `{ source, source_name, target, target_name, kind, amount, crit, miss, text }` | удар; `kind` = melee\|skill\|rune\|scroll; `text` — серверный, рендерится как есть |
| `evt/combat/cooldown` | `{ skill_id?, until_epoch }` | блокировка действия |
| `evt/combat/end` | `{ reason }` | конец боя |
| `evt/magic/cast` | `{ by, by_name, spell, what: "scroll"\|"rune", target?, charges_left?, mp_left?, ok }` | каст (свой/чужой); `charges_left` — заряды руны, `mp_left` — остаток маны |

### Предметы
| Событие | Payload | Назначение |
|---|---|---|
| `evt/item/added` | `{ item: ItemSnapshot }` | предмет получен |
| `evt/item/removed` | `{ uid, reason }` | потерян/израсходован (`reason` = consumed\|traded\|dropped\|lost_on_death) |
| `evt/item/state` | `{ uid, patch }` | патч предмета (upgrade, sockets, qty) |
| `evt/upgrade/result` | `{ uid, ok, new_upgrade?, item_destroyed?, cost {} }` | итог улучшения |
| `evt/craft/result` | `{ recipe_id, ok, made: [{uid, qty}], consumed [] }` | итог крафта |
| `evt/craft/profession` | `{ profession: { id, rank, xp, xp_to_next } }` | профессия изучена/ранг повышен |
| `evt/quest/progress` | `{ quest: QuestState }` | прогресс цели |
| `evt/quest/updated` | `{ quest: QuestState }` | смена состояния (взят/сдан/доступен) |

### Преступность (см. §10)
| Событие | Payload | Назначение |
|---|---|---|
| `evt/crime/status` | `{ statuses: [CrimeStatus], log_hint }` | **полная замена** списка статусов |
| `evt/crime/added` | `{ status, log_entry }` | новый срок (дополняет журнал) |
| `evt/crime/removed` | `{ kind }` | статус истёк (fallback: клиент снимает по `expires_at`) |

### Социальное
| Событие | Payload | Назначение |
|---|---|---|
| `evt/chat/message` | `{ channel, from, name, text, tags[]?, sent_at (ISO-8601) }` | сообщение чата |
| `evt/party/updated` | `{ party: PartyState \| null }` | полное состояние группы (список, лидер, HP) |
| `evt/trade/state` | `{ offer_id, version, you: ItemSnapshot[], them: ItemSnapshot[], you_gold, them_gold, you_locked, them_locked, value_hint? }` | каждое изменение = новый `version` |
| `evt/trade/closed` | `{ offer_id, reason: "done"\|"cancelled"\|"failed", result? }` | завершение сделки |
| `evt/guild/updated` | `{ guild: GuildState \| null }` | полное состояние гильдии |
| `evt/guild/log` | `{ entry: { kind, text, at, … } }` | событие гильдии (вступление, ранг), `at` — epoch |

---

## 9. Коды ошибок

`ack.error.code` — UPPER_SNAKE. Клиент МУСТ иметь локализованный текст на каждый код; поле `message`
(русский текст сервера) — резервный вариант отображения.

| Код | Когда |
|---|---|
| `E_AUTH_EXPIRED` | access/refresh истёк → экран входа |
| `E_BAD_CREDENTIALS` | неверный логин/пароль |
| `E_LOGIN_TAKEN` | логин уже занят (`auth.register`) |
| `E_WEAK_PASSWORD` | логин/пароль короче лимитов сервера (`auth.register`) |
| `E_REGISTER_DISABLED` | регистрация выключена настройкой сервера |
| `E_BAD_EMAIL` | email пуст/невалиден по синтаксису (`auth.register`, D33) |
| `E_EMAIL_TAKEN` | email уже занят другим аккаунтом (`auth.register`, D33) |
| `E_CHAR_LIMIT` | слотов персонажей больше нет (`char_slot_limit`) |
| `E_NAME_TAKEN` | имя занято/невалидно |
| `E_BAD_PROFILE` | неизвестный/недоступный `profile` в `char.create` |
| `E_NOT_IN_LOCATION` | команда вне контекста локации |
| `E_EXIT_BLOCKED` | выход закрыт (завал, требование, правило) |
| `E_TARGET_GONE` | цели нет (умерла/ушла) |
| `E_NOT_IN_COMBAT` / `E_IN_COMBAT` | бой разрешён/запрещён здесь |
| `E_COOLDOWN` | действие на кулдауне |
| `E_NO_CHARGES` | у руны нет зарядов |
| `E_NOT_ENOUGH_MP` | не хватает маны для каста (D4: руна тратит ману + заряд) |
| `E_NO_RUNE_SLOT` | предмет без свободного/подходящего сокета |
| `E_REQUIREMENT_ATTR` | не хватает атрибута/уровня |
| `E_PROFESSION` | нет требуемой профессии |
| `E_INVENTORY_FULL` | инвентарь заполнен |
| `E_ITEM_GONE` | предмета уже нет (двойное действие/обмен) |
| `E_UPGRADE_PREVIEW_STALE` | `preview_id` устарел (пересоздать превью) |
| `E_UPGRADE_MAX` | достигнут максимум улучшения |
| `E_TRADE_STALE` | `version` сделки не совпал → перерисовать окно |
| `E_TRADE_PARTNER_GONE` | партнёр ушёл |
| `E_PARTY_FULL` / `E_GUILD_LIMIT` | лимиты группы/гильдии |
| `E_GUILD_PERMISSION` | нет прав (ранг) |
| `E_XP_POOL` | не хватает очков опыта (`char.xp_invest`) |
| `E_RANK_CAP` | достигнут потолок ранга навыка/профессии |
| `E_PROFESSION_LIMIT` | превышен `profession_limit` первичных профессий |
| `E_STATION_REQUIRED` | для рецепта нет станка в локации |
| `E_TOOL_MISSING` | нет требуемого инструмента в инвентаре |
| `E_INGREDIENTS` | не хватает ингредиентов для рецепта |
| `E_RATE_LIMITED` | троттлинг (+ `retry_after_s`) |
| `E_PVP_FORBIDDEN` | действие запрещено правилами зоны |
| `E_TOO_LARGE` | фрейм > 64 KiB |
| `E_INTERNAL` | внутренняя ошибка (логировать на сервере, показать общий текст) |

---

## 10. Преступность: модель частичного сложения (серверная)

Клиент **не пересчитывает** сроки — только рендерит `statuses[]` (§8) и строки журнала (`log_hint`,
`evt/crime/added.log_entry`). Формула ниже — рекомендуемая модель для мока/сервера, заменяемая
конфигом (`crime.*` из §5), **контракт это допускает** — клиент обязан съедать любые значения.

Обозначения: `T` — текущий общий срок (сек), `t` — срок нового преступления, `T_MAX` — потолок, `k = blend_k`.

```
T_new = min( T_MAX ,  T + k · t · (1 − T / T_MAX) )
```

- `kind`-ы сосуществуют независимо (у каждого свой `expires_at`); формула — эскалация общего срока.
- `k = 1.0` → обычное сложение; `k < 1` → каждый следующий нарушитель вносит меньше; при `T → T_MAX` вклад → 0.
- Пример (k=0.5, T_MAX=604800, murderer=21600, marauder=7200): 21600 → затем +0.5·7200·(1−21600/604800) ≈ **24741 с** (≈6 ч 52 м).
- События сервера: `evt/crime/added` (новый статус/срок), `evt/crime/status` (замена списка),
  `evt/crime/removed` (истечение) + подстраховка клиентом по `expires_at` (см. §8).

---

## 11. Версионирование и совместимость

1. `v` в конверте — wire-версия протокола. Сейчас `v = 1`.
2. **Совместимо** (без bump): новые опциональные поля, новые `evt/*`/`cmd/*`, новые значения в списках (`features`, `kinds`, `channels`), новые `error.code` (клиент показывает fallback по `message`).
3. **Несовместимо** (bump `v → 2`): удаление/переименование полей и команд, смена типов, изменение семантики.
4. `v > 1` на клиенте `v=1` → клиент показывает «Требуется обновление клиента» (блокирует вход, кроме dev-сборки).
5. Незнакомый `type` — игнорировать с логом (никогда не ронять).
6. Типы — только в `ld2-protocol`; правки контракта = PR `[tz-amend]` + golden-тесты **до** изменений клиента.

---

## 12. Примеры диалогов

### 12.1 Вход и снапшот

```jsonc
→ { "v":1, "type":"cmd/auth.login", "seq":1, "ts":"…", "payload": { "login":"demo","password":"demo123" } }
← { "v":1, "type":"ack", "seq":1, "state_version":null, "result":"ok",
    "payload": { "access_token":"at_…","expires_in_s":900 } }
← { "v":1, "type":"evt/session.start", "ts":"…", "payload": { "/*": "см. §5" } }
→ { "v":1, "type":"cmd/char.select", "seq":2, "ts":"…", "payload": { "char_id":"c1" } }
← { "v":1, "type":"ack", "seq":2, "state_version":1337, "result":"ok" }
← { "v":1, "type":"evt/state/snapshot", "ts":"…", "state_version":1337, "payload": { "/*":"см. типы" } }
```

### 12.2 Переход и бой

```jsonc
→ { "v":1, "type":"cmd/location.move", "seq":3, "payload": { "exit_id":"e_north" } }
← { "v":1, "type":"ack", "seq":3, "state_version":1338, "result":"ok",
    "payload": { "location": { "id":"l_forest","name":"Черный лес","flags":["pvp"],
      "text":"Сосны давят тьмою…","exits":[{"id":"e_s","label":"тропа на юг"}],
      "entities":[{"id":"m_3","kind":"monster","name":"Волк","hp":{"cur":18,"max":18}}] } } }
→ { "v":1, "type":"cmd/combat.attack", "seq":4, "payload": { "target_id":"m_3" } }
← { "v":1, "type":"ack", "seq":4, "state_version":1339, "result":"ok" }
← { "v":1, "type":"evt/combat/hit", "ts":"…", "state_version":1339, "payload":
    { "source":"c1","source_name":"Ким","target":"m_3","target_name":"Волк","kind":"melee",
      "amount":7,"crit":false,"miss":false,"text":"Вы бьете волка на 7." } }
```

### 12.3 Мародёрство → статус

```jsonc
→ { "v":1, "type":"cmd/entity.info", "seq":9, "payload": { "entity_id":"cor_9" } }
← ack (труп игрока, loot_count:3, was_player:true)
→ { "v":1, "type":"cmd/item.use", "seq":10, "payload": { "item_uid":"loot:cor_9:0" } }  // взять лут
← { "v":1, "type":"ack", "seq":10, "state_version":1345, "result":"ok",
    "payload": { "item": { "uid":"i_44", "type":"weapon","name":"Кинжал","rarity":"common" } } }
← { "v":1, "type":"evt/corpse/looted", "ts":"…", "payload":
    { "corpse_id":"cor_9","by":"c1","by_name":"Ким","was_player":true } }
← { "v":1, "type":"evt/crime/added", "ts":"…", "payload":
    { "status": { "kind":"marauder","severity":"medium","expires_at":1762147511 },
      "log_entry": { "kind":"marauder","at":1762140311,"delta_s":7200,"total_s":24741 } } }
← { "v":1, "type":"evt/crime/status", "ts":"…", "payload":
    { "statuses":[ {"kind":"murderer","severity":"high","expires_at":1762150000},
                   {"kind":"marauder","severity":"medium","expires_at":1762147511} ],
      "log_hint":"За мародёрство: +2ч 00м (итого срок 6ч 52м)" } }
```

### 12.4 Reconnect

```jsonc
(связь потеряна)
→ cmd/auth.resume { refresh_token } → ack + evt/session.start
→ cmd/state.resync { last_state_version: 1345 }
← evt/state/snapshot { state_version: 1380, … }   // или evt/state/delta, если возможно
```

### 12.5 Улучшение предмета

```jsonc
→ { "v":1, "type":"cmd/upgrade.preview", "seq":21, "payload": { "item_uid":"i_17" } }
← ack { "preview_id":"pv_7","delta":{"magic":"+2"},"cost":{"gold":300,"materials":{"rune_dust":2}},
        "chance":0.75 }
→ { "v":1, "type":"cmd/upgrade.start", "seq":22, "payload": { "item_uid":"i_17","preview_id":"pv_7" } }
← ack { state_version: 1381, result: ok }
← { "v":1, "type":"evt/upgrade/result", "ts":"…", "state_version":1382, "payload":
    { "uid":"i_17","ok":true,"new_upgrade":3,"item_destroyed":false,
      "cost":{"gold":300,"materials":{"rune_dust":2}} } }
← { "v":1, "type":"evt/item/state", "ts":"…", "state_version":1382,
    "payload": { "uid":"i_17","patch": { "upgrade":3 } } }
```

---

## 13. Чек-лист автора сервера (мок или реальный)

- [ ] Протокол `v=1`, `Sec-WebSocket-Protocol: ld2.v1`, лимит фрейма 64 KiB, текстовые JSON.
- [ ] `seq`-окно 128 с кэшированными ack; ровно один ack на команду; события после своих ack.
- [ ] `state_version` монотонный на сессию персонажа; snapshot на select/resume; delta/снапшот по `resync`.
- [ ] `ts` = ISO-8601, игровая истина времени — epoch-секунды; `time.sync` отвечает.
- [ ] Всё содержимое лимитов/фич — в `evt/session.start` (§5), без «сюрпризов» в коде клиента.
- [ ] Коды ошибок §9; локализуемый `message` по-русски; `retry_after_s` там, где есть троттлинг.
- [ ] `cmd/auth.register`: лимиты длины из конфига (`E_WEAK_PASSWORD`), уникальность
      (`E_LOGIN_TAKEN`), выключенный флаг (`E_REGISTER_DISABLED`), rate-limit → `E_RATE_LIMITED`;
      email: синтаксис/нормализация (`E_BAD_EMAIL`), уникальность (`E_EMAIL_TAKEN`, D33).
- [ ] Модель преступности §10 через конфиг `crime.*`; статусы включаются присутствием `kind` в `crime.kinds` (в моке: murderer, marauder, **thief**).
- [ ] Мана (D4): сервер проверяет `mp` перед кастом руны (`E_NOT_ENOUGH_MP`), свитки — без маны; реген по `magic.mana` из §5.
- [ ] Неизвестные `cmd` от клиента (старее/новее по версии) — `E_INTERNAL`/игнор по §11, без падений.

## 14. Чек-лист автора клиента

- [ ] Неизвестные `type`/поля игнорируются с логом; `v>1` → «обновите клиент».
- [ ] Таймаут ack 10 с, ретраи только идемпотентных команд (§7), backoff/reconnect по `CONTRIBUTING`/`tz` §6.3.
- [ ] Время: `time_offset` из `evt/time.sync`, отсчёты по серверным `expires_at`, пересчёт после снапшота.
- [ ] Токены только в keyring; логи без паролей/токенов.
- [ ] Все лимиты из §5 (никаких `MAX_CHARS` в коде).
- [ ] `evt/crime/status` — полная замена; fallback-истечение по `expires_at`; **никаких собственных расчётов blend**.
- [ ] Мана: mp-бар из `char.mp`; при `E_NOT_ENOUGH_MP`/`E_NO_CHARGES` — приглушение иконки с причиной; `mp_left`/`charges_left` из `evt/magic/cast` — отображение бейджей.

---

*Канонический файл контракта. Краткое изложение — `tz.md` §4; типы — crate `ld2-protocol`.
Правки контракта — PR `[tz-amend]` + обновление golden-тестов и, если нужно, этой документации.*

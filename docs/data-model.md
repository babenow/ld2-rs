# Модель данных ld2

**Связи:** [`tz.md`](./tz.md) §6 (контент/мир), [`protocol.md`](./protocol.md)
(снапшоты и события), [`skills.md`](./skills.md) (навыки/опыт),
`content/*.json` — исходники контента, `crates/ld2-server/src/db/` — доступ к БД.

Сущности живут в **четырёх слоях**:

1. **Контент** (`content/*.json`) — статика мира, грузится при старте сервера
   и сеется в PostgreSQL (кроме `dialogs.json`/`shops.json` — они в памяти);
2. **PostgreSQL** — персистентное состояние аккаунтов, персонажей и прогресса;
3. **Wire-типы** (`ld2-protocol`) — что летит клиенту в снапшотах/событиях;
4. **Клиентский store** (`ld2-core`) — производные вкладки окон (диалог, магазин,
   обучение).

---

## 1. Контент (`content/*.json`)

### 1.1 `items.json` → предметы (58 определений, сид в `item_defs`)

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `name` | str | ключ и отображаемое имя |
| `type` | str | `weapon`, `staff`, `shield`, `armor`, `gem`, `rune`, `artifact`, `money`, `potion`, `scroll`, `herb`, `wood_oak`, `parchment`, `ink`, `flask`, `chisel`, `quill` |
| `rarity` | str | `common` / `uncommon` / `rare` / `epic` |
| `slot` | str? | слот экипировки: `main_hand`, `off_hand`, `head`, `body`, `legs`, `feet`, `gloves`, `shoulders`, `bracers`, `cloak`, `ring`, `amulet`, `artifact` |
| `qty` | int | размер стака (партия выдачи/продажи) |
| `upgrade` | int | предельный уровень улучшения |
| `gold_value` | int | база цены (наценки магазина считаются от неё) |
| `bonuses` | list? | **бонусы предмета** (M18/F47, ADR-010): `[{kind, id, value, pct?}]`, `kind ∈ attr \| param \| skill` — «Сила +3», «Точность +4%», «+1 к рангу навыка»; `pct` только у `param` |
| `stats` | obj | `damage`/`dmg`/`armor`/`heal`/`charges`/`spell`/`school`/`magic`/`value`/`attr`/`max` |
| `requires` | obj? | требования использования (`attr`, `level`, `profession`) |
| `sockets` | int? | число сокетов (руны → оружие, самоцветы → броня) |
| `hands` | int? | «две руки» для оружия |
| `skill` | str? | нужный навык (гейт использования) |
| `cooldown_s` | float? | кулдаун использования (дефолт 1 с) |
| `hits_as` | str? | **«чем бьёт»** (M18/F48): творительный для строки лога (`мечем`, `секирой`, `из лука`); только у `weapon`/`staff`; `null` → фолбэк по навыку/типу |

### 1.2 `skills.json` → навыки (30, сид в `skills`)

`id`, `name`, `group` (группы A–F по GDD §3), `description`.
Прогресс живёт отдельно: `character_skills.xp` (шкала) + `.rank`.

### 1.3 `professions.json` → профессии (8, сид в `professions`)

`id`, `name`, `type`.

### 1.4 `recipes.json` → рецепты (сид в `recipes`)

`id`, `name`, `profession`, `rank`, `station` + `station_label`, `tool{type}`,
`requires{}`, `inputs[{type, qty}]`, `time_s`, `xp{profession}`,
`output{type, qty}`.

### 1.5 `quests.json` → квесты (сид в `quests`)

`id`, `title`, `giver` (NPC-выдающий),
`goals[{type, target, need, text}]`,
`reward{xp, items[{type, qty}]}`, `reward_label`.

Состояние квеста персонажа — `character_quests.state`
(`available → active → turn_in → done`) + `progress jsonb`.

### 1.6 `locations.json` → мир (сид в `locations`)

```
location: { id, name, spawn, flags[], text,
            exits[{ id, label, dir, to }],
            entities[{ id, kind, name, hp{cur,max}, tags[], aggro,
                       difficulty, drops[], loot[], was_player,
                       station, trainer{professions[], skills[],
                                        price{gold_base, gold_per_rank,
                                              items[{type, qty}]}} }] }
```

- `kind` ∈ `player | monster | npc | corpse | object`;
- `trainer` — маркер M14: NPC обучает профессиям/навыкам локации
  (гейт `E_NO_TRAINER`); **цена обучения навыков** при этом берётся из
  `dialogs.json → training` (ADR-009), маркер — только гейт;
- `was_player` — ключ маркера трупа игрока (статус «Мародёр»).

### 1.7 `dialogs.json` → диалоги NPC (в памяти, M18)

```
npc: { id,
       root,                                  # корень ветки
       training[{ skill, points, gold,        # чему учит и по какой цене
                  items[{def_id, qty}] }],    # ADR-009
       lines[{ id, text,
               quest?,                        # строка несёт квест
               every_s?,                      # кулдаун показа, сек
               when{ gender | level | attr | skill | profession | item },
               replies[{ id, text, next?,     # next: null — конец ветки
                         when?,               # условия ответа (AND)
                         action? }] }] }      # action: "shop" | "train"
```

- ответ, чья целевая строка на `every_s`, показывается **неактивным с
  таймером** (`wait_s`, ADR-008); скрытые условиями ответы не показываются;
- `action` открывает панель магазина/обучения вместо перехода (F43/F44).

### 1.8 `shops.json` → магазины (в памяти, M18)

```
npcs[{ id, markup,                        # наценка торговца
       currencies[{ currency,             # def_id валюты (gold_coin, gem_*)
                    rate,                 # единиц валюты за золотой эквивалент
                    items[] }] }]         # товары вкладки
```

Итоговая цена = `gold_value × server_markup × markup × (1 − скидка bp)`,
затем `× rate` (конвертация валюты).

---

## 2. PostgreSQL — персистентное состояние (21 таблица)

### 2.1 Аккаунты и персонажи

| Таблица | Ключевые поля |
|---|---|
| `accounts` | `id`, `login`, `password_hash` (argon2id), `email`, `role`, `banned_until`, `last_char_id`, `shop_discount_bp` (скидка премиума, bp) |
| `refresh_tokens` | `account_id → accounts`, `token_hash`, `expires_at`, `revoked_at` |
| `characters` | `account_id → accounts (CASCADE)`, `name`, `profile` (`warrior`/`archer`/`mage`), `gender`, `location_id`, `hp_cur/hp_max`, `mp_cur/mp_max`, **`xp_pool`** (очки опыта), **`total_xp`** (весь опыт → уровень), `state_version`, `attrs jsonb`, `deleted_at` |

### 2.2 Инвентарь и прогресс (все — по `characters`, CASCADE)

| Таблица | Ключевые поля |
|---|---|
| `items` | `uid`, `def_id → item_defs`, `qty`, `upgrade`, `equipped_slot`, `sockets jsonb`, `sort_order`, **`bonuses jsonb`** (бонусы инстанса — задел под источники в игре: кузница/награды) |
| `item_defs` | сид контента: `id`, `item_type`, `name`, `rarity`, `upgrade`, `stats jsonb`, `requires jsonb`, `gold_value`, `qty`, `skill`, `sockets`, `slot`, `hands`, `cooldown_s`, **`bonuses jsonb`** (контент-бонусы), **`hits_as text`** («чем бьёт», M18/F48) |
| `character_skills` | `(character_id, skill_id)`, **`xp`** (шкала копится использованием), **`rank`** (заученный; **эффективный ранг** = `rank` + бонусы `kind=skill` надетых предметов — считается при построении снапшота, в БД не пишется) |
| `character_professions` | `(character_id, profession_id)`, `xp`, `rank` |
| `character_quests` | `(character_id, quest_id)`, `state`, `progress jsonb` |
| `character_dialog_lines` | `(character_id, npc_id, line_id)`, `shown_at` — кулдауны `every_s` диалогов |

### 2.3 Крафт, мир, соц, служебные

| Таблица | Ключевые поля |
|---|---|
| `craft_jobs` | `character_id`, `recipe_id`, `qty`, `output_*`, `finishes_at` (таймер готовности) |
| `quests`, `recipes`, `skills`, `professions`, `locations` | сиды контента |
| `crime_statuses` | `(character_id, kind)`, `expires_at` — статусы «Убийца/Мародёр/…» |
| `crime_log` | `character_id`, `kind`, `at`, `delta_s`, `total_s` — журнал нарушений |
| `guilds`, `guild_members`, `guild_log` | гильдии: состав, ранги, журнал |
| `chat_history` | `channel`, `from_char`, `name`, `text`, `sent_at` — кольцевая история |
| `loot_taken` | `uid`, `character_id`, `taken_at` — труп не фармится дважды |
| `meta`, `_sqlx_migrations` | служебные |

**Связи:** `characters → accounts` (CASCADE), `items`/`character_*` → `characters`
(CASCADE), `guild_members → guilds/characters`, `craft_jobs → characters`.

---

## 3. Wire-типы (`ld2-protocol`) — что получает клиент

| Тип | Содержимое |
|---|---|
| `CharacterState` | `id`, `name`, `attrs{}`, `skills[]`, `professions[]`, `hp`, `mp`, `xp_pool`, `level`, `total_xp`, `xp_in_level`, `xp_to_next`, `derived`, `profile`, `gender`, `equip{слот → ItemSnapshot}`, `inventory[]`, `crime`, `quests[]` — ядро `evt/state/snapshot` |
| `ItemSnapshot` | `uid`, `item_type`, `name`, `rarity`, `upgrade`, `stats{}`, `requires?`, `sockets[]`, `def_id`, `slot`, `hands`, `qty`, **`bonuses?`** — карточка предмета, позиция магазина, лут |
| `ItemBonus` | `{kind: attr\|param\|skill, id, value, pct?}` — бонус надетого предмета (ADR-010): `attr` → в `CharacterState.attrs`, `param` → `stats::derive` (плоские к итогу, `%` — сумма к итогу), `skill` → +N к рангу `SkillSnapshot.rank` |
| `SkillSnapshot` | `id`, `name`, `description`, `group`, `rank`, `xp`, `xp_to_next` — `rank` включает **бонусы надетых предметов** (`kind=skill`), а `xp_to_next` считается по заученному рангу — шкала и `skill.train` согласованы |
| `ProfessionSnapshot` | `id`, `rank`, `xp`, `xp_to_next` |
| `QuestState` | `id`, `title`, `state` (`available\|active\|turn_in\|done`), `goals[]`, `reward_preview`, `reward_label` |
| `Entity` / `LocationSnapshot` | `id`, `kind`, `name`, `hp`, `tags`, `aggro`, `extra{trainer…}` / `flags`, `exits[]`, `entities[]` |
| `CrimeState` / `CrimeLogEntry` | `statuses[{kind, expires_at}]` / `{kind, at, delta_s, total_s}` |

**Диалог (M18):** `npc.talk` →
`{dialog:{id, name}, node:{id, text, quest?, replies:[{id, text, wait_s?, action?}]},
quests[], actions[], training:[{skill, points, gold, items?}]}`.

**Команды обучения (ADR-009):** `cmd/skill.train {entity_id, skill_id}` →
ack `{rank, xp, xp_to_next, pool_left}` + `evt/char/xp` (отрицательный `amount`).

---

## 4. Клиентский store (`ld2-core`) — производные вкладки

| Тип | Поля |
|---|---|
| `NpcDialogView` | `id`, `name`, `node`, `transcript[]`, `quests[]`, `actions[]`, `shop?`, `shop_currency?`, `training[]`, `training_open` — всё чистится при закрытии окна |
| `NpcNode` / `NpcReplyOption` | `replies[{id, text, wait_s, wait_until_ms, action}]` (`wait_until_ms` — живой отсчёт таймера, wire не несёт) |
| `NpcShopView` | `tabs[{currency, currency_name, items[{def_id, name, rarity, price, qty, item}]}]` |
| `NpcTraining` | `skill`, `points`, `gold`, `items[{def_id, qty}]` |
| `NpcShopItem` | `item: Option<ItemSnapshot>` — карточка по клику (F40) |

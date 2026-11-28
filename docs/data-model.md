# Модель данных ld2

**Актуально:** workspace `0.22.0`, ветка `develop`; сверено с кодом 2026-11-22.

**Связи:** [`tz.md`](./tz.md) §6 (контент/мир), [`protocol.md`](./protocol.md)
(снапшоты и события), [`skills.md`](./skills.md) (навыки/опыт),
[`professions.md`](./professions.md) (профессии/крафт), `content/*.json` —
исходники контента, `crates/ld2-server/src/db/` — доступ к БД.

Сущности живут в **пяти слоях**:

1. **Контент** (`content/*.json`) — статика мира; шесть файлов сидятся в
   PostgreSQL, `dialogs.json`/`shops.json` остаются в памяти;
2. **PostgreSQL** — аккаунты, персонажи, инвентарь и прогресс;
3. **Рантайм мира** (`ld2-server/src/world.rs`) — боевые мобы, трупы,
   ресурсы, контейнеры и предметы на земле; живёт в памяти и собирается
   заново при рестарте;
4. **Wire-типы** (`ld2-protocol`) — то, что летит клиенту в снапшотах и
   событиях;
5. **Клиентский store** (`ld2-core`) — производные вкладки окон (диалог,
   магазин, обучение, карта, подбор).

---

## 1. Контент (`content/*.json`)

### 1.0 Файлы, загрузка и версия

| Файл | Содержимое | Куда попадает |
|---|---|---|
| `locations.json` | 1194 локации | `locations` |
| `items.json` | 262 определений | `item_defs` |
| `skills.json` | 34 навыка | `skills` |
| `professions.json` | 14 профессий | `professions` |
| `recipes.json` | 99 рецептов | `recipes` |
| `quests.json` | 1 квест | `quests` |
| `dialogs.json` | 5 NPC | только память |
| `shops.json` | 5 NPC | только память |

`CONTENT_FILES` в `crates/ld2-content/src/load.rs` — ровно эти восемь
файлов. `load()` читает шесть обязательных в `GameContent`; пустой
`locations` — `ContentError::Empty`. Диалоги и магазины грузятся отдельно
(`Dialogs`/`Shops`) и в БД не сеются.

`locations.json` — активная карта: 9 учебных локаций (`locations_main.json`)
+ 1180 Ренмарка (`locations_renmark.json`). `locations_ad.json` (1702
локации) — отдельный легаси-фрагмент ад-мира, в активную карту не входит.
Флаг `spawn: true` стоит у `l_village` и `l_velgard_sloboda_01`; сервер
берёт **первую** такую локацию (`content.rs`), сейчас это `l_village`, и
пишет её в `meta.spawn_location`. После переноса спавна на
`l_velgard_sloboda_01` корнем карты станет она.

Флаги локаций: только `safe` и `pvp`. Выходы: `dir` — `north`/`east`/
`south`/`west` или отсутствует; `to` обязателен в контенте, в wire-тип
`Exit` не попадает.

**Версия контента.** `version_raw()` в `crates/ld2-content/src/version.rs`
считает sha256 по каноническому JSON всех восьми файлов (ключи — имена без
`.json`). Форматирование и порядок ключей версию не меняют. Сервер
сравнивает её с `meta.content_version`; при совпадении БД не трогается
(M12.3), при изменении — пересид. Заголовок `/healthz`:
`x-ld2-content-version`. Проверка контента:
`target/debug/ld2-editor --check content` (8 файлов, ошибки/предупреждения).

**Открытая схема.** У всех структур контента есть
`#[serde(flatten)] extra: Map<String, Value>`: неизвестные поля
сохраняются, редактор их не теряет. Валидация проверяет обязательные поля
и типы, но не запрещает расширения.

### 1.1 `items.json` → предметы (262, сид в `item_defs`)

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `name` | str | ключ и отображаемое имя |
| `description` | str? | текст для карточки/тултипа (M39; см. [`items.md`](./items.md)) |
| `personal` | bool? | при первом надевании/применении становится персональным (M39) |
| `type` | str | тип предмета (см. ниже) |
| `rarity` | str | `common` / `uncommon` / `rare` / `epic` / `legendary` |
| `upgrade` | int | **базовый** уровень предмета (0 у обычных, 10 у `sword_1000_true`) |
| `stats` | obj | открытая карта характеристик |
| `requires` | obj? | `{attr, level?, profession?}` — гейт использования |
| `gold_value` | int | база цены; от неё считаются наценки магазина |
| `qty` | int? | размер стака/партии выдачи |
| `skill` | str? | навык-гейт использования |
| `sockets` | int | всего сокетов |
| `rune_sockets` | int? | сколько из них принимают руны; `null` — по типу |
| `cooldown_s` | float? | кулдаун использования (в БД дефолт 1.0) |
| `slot` | str? | слот экипировки |
| `hands` | int? | 1/2 руки |
| `bonuses` | list | `[{kind, id, value, pct?}]`, `kind ∈ attr \| param \| skill` |
| `extra` | obj | неизвестные поля |

Типы (262): `gem` 117, `armor` 23, `rune` 18, `weapon` 11, `bag` 8,
`hide` 6, `hide_scraps` 5, `shield` 3, `upgrade_stone` 3, `staff` 2,
`artifact` 4, `pickaxe` 2, `skinning_knife` 2; `raw_*` 32
(12 смешанных + 20 чистых), `ore_*` 9
(`ore_copper`…`ore_mithril`, включая `ore_bronze`, `ore_platinum`,
`ore_adamantite`); `wood_oak`, `sickle`, `scroll`, `quill`, `potion`, `parchment`, `offal`, `money`,
`meat`, `ink`, `herb`, `flask`, `chisel`, `branches`, `axe`, `ammo`,
`key` — по 1.

Редкости: `common` 72, `uncommon` 76, `rare` 55, `epic` 52,
`legendary` 7.

Слоты (13): `main_hand`, `off_hand`, `head`, `body`, `legs`, `feet`,
`gloves`, `shoulders`, `bracers`, `cloak`, `ring`, `amulet`, `artifact`.
В конфиге экипировки слотов 15: `ring` разворачивается в `ring1`/`ring2`,
`artifact` — в `artifact1`/`artifact2`.

**`stats`.** Ключи: `armor`, `attr`, `buff`, `buff2`, `chance`, `charges`,
`class`, `cost`, `damage`, `dmg`, `effect`, `glyph`, `heal`, `magic`, `max`,
`param`, `quality`, `school`, `spell`, `value`. Важнейшие формы:

- `dmg` / `damage` — число или диапазон строкой (`"13..26"`);
- `attr` — имя атрибута (`str`/`dex`/`vit`/`int`), сила — в `value`;
- `param` — карта `param → число` или `param → {value, pct}`;
- `buff` — `{param, value, pct?, duration_s, max_stacks}`;
- `effect` — `{kind: stun|root|bleed|burn, value?, duration_s, period_s?}`;
- `spell`, `school`, `class`, `glyph`, `charges`, `max`, `quality`, `cost`,
  `chance`, `buff2` — метаданные рун/магии.

**Бонусы.** В wire `ItemSnapshot.bonuses` попадают:
`def.bonuses` + `stats.param` (число или `{value,pct}`) + `items.bonuses`
инстанса. `stats.attr` в этот список не входит — атрибуты считаются
отдельно. `pct: true` — процентный бонус, иначе абсолютный.

**Сокеты.** `sockets` — общее число; `rune_sockets` — сколько первых
слотов принимают руны. `rune_sockets = null`: броня — 0 (все под
самоцветы), остальные типы — все слоты рунные. В wire у каждого
`Socket` есть `accepts: "rune" | "gem"`; если в сырой записи его нет, он
добирается при отправке по индексу и `rune_sockets`. Пустой
`items.sockets` при `def.sockets > 0` разворачивается в список пустых
слотов — окно инкрустации видит все слоты до первой вставки.

**Улучшение.** `item_defs.upgrade` — базовый уровень, `items.upgrade` —
дельта инстанса. В снапшот идёт сумма; потолок суммарного уровня —
`gameplay.upgrade.max_level = 9`. Базовый уровень из контента может быть
выше потолка (легендарный `sword_1000_true` уже `+10`) — такой предмет
дальше не улучшается.

Примеры:

| `id` | Что показательно |
|---|---|
| `gold_coin` | `type: money`, `gold_value: 1`, `qty: 10`; деньги — предметы, отдельного `characters.gold` нет |
| `drakkar_axe` | `epic`, `dmg: "13..26"`, `param.crit_chance: 3`, бонусы `str+3`/`damage+2`/`accuracy+4%`/`sword+1`, `twohand`, `main_hand`, 2 руки, 4 сокета (2 рунных) |
| `sword_1000_true` | «Клеймор тысячи истин», `legendary`, двуручный, базовый `upgrade: 10`, `dmg: "19..31"`, бонусы `str+5`/`dex+5`/`damage+6`/`accuracy+4%`/`crit_chance+12%`/`sword+1` |
| `sword_1000_true_1h` | «Клинок тысячи истин», `legendary`, одноручный, `upgrade: 10`, `dmg: "15..24"`, 4 сокета (2 рунных) — в паре со щитом |
| `shield_1000_true` | «Щит тысячи истин», `legendary`, `off_hand`, `armor: 15`, `block 12..18`, `block_chance 12%`, 5 сокетов (2 рунных + 3 самоцветных) |
| `pelt_white_wolf` | «Шкура белого волка», `rare`, `hide`; редкий трофей со снятия шкуры Белого волка — `harvest.skin_extra`, шанс 3 % |
| `shield_wood` / `shield_iron` | базовый шанс блока `5%` / `8%` (`stats.param.block_chance`); сила и ловкость добавляют `(str+dex)/10` к общему шансу |
| `ore_tin` … `ore_mithril` | руды 6 уровней; тип = id руды, добываются киркой |
| `raw_agate` … | необработанные смешанные самоцветы (12) |
| `raw_stone_armor` … | необработанные чистые камни (20 параметров) |
| `gem_agate_1/2/3` | обработанные самоцветы: `bonuses` attr+param, 3 версии |

### 1.2 `skills.json` → навыки (34, сид в `skills`)

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `name` | str | ключ и имя |
| `group` | str | группа в окне навыков |
| `description` | str | описание (дефолт `""`) |
| `extra` | obj | расширения |

Группы: `weapon` 8 (`sword`, `axe`, `polearm`, `bow`, `dagger`, `twohand`,
`unarmed`, `power_strike`), `defense` 9 (`armor_heavy`, `armor_light`,
`block`, `evasion`, `parring`, `shield_block`, `m_evasion`, `m_armor`,
`m_dispel`), `magic` 6 (`fire`, `frost`, `storm`, `healing`, `arcana`,
`dark`), `practice` 4 (`mining`, `woodcutting`, `survival`, `bestiary`),
`stealth` 4 (`stealth`, `lockpicking`, `pickpocket`, `disarm`), `social` 3
(`haggle`, `lore`, `leadership`).

Прогресс — `character_skills (character_id, skill_id, xp, rank)`. Ранг
растёт только при заполненной шкале и оплате у тренера; потолок
`[skills] rank_max = 30`, `xp_to_next = round(base_xp × growth^rank)`
(`base_xp = 120`, `growth = 1.32`). Гейты атрибутов: ранг 10 → 12, 20 → 18,
30 → 25 (максимальный атрибут персонажа). Эффективный ранг в
`SkillSnapshot` включает бонусы предметов `kind: skill`.

### 1.3 `professions.json` → профессии (14, сид в `professions`)

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `name` | str | ключ и имя |
| `type` | str | `primary` (дефолт) / `secondary` |
| `extra` | obj | расширения |

`primary` 11: `herbalist`, `alchemist`, `mining`, `blacksmith`, `skinning`,
`leatherworking`, `tailor`, `scribe`, `woodcutting`, `woodcarver`,
`jeweler`. `secondary` 3: `cook`, `fishing`, `medic`.

Прогресс — `character_professions (character_id, profession_id, xp, rank,
level)`. Две шкалы: **уровень** 0…250 и **ранг** 1…5; ранг выводится из
уровня с шагом `rank_span = 50` (`ceil(max_level / rank_span)`). Основных
профессий у персонажа не больше `profession_limit = 2`; вторичные слот не
занимают. Повышение ранга — у тренера за золото
(округление `rank_up_gold_base × 1.5^rank`, база 200) или наградой квеста
(`reward.rank_up`), без золота.

### 1.4 `recipes.json` → рецепты (3, сид в `recipes`)

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `name` | str | ключ и имя |
| `profession` | str | профессия-исполнитель |
| `rank` | int | минимальный ранг профессии (дефолт 1) |
| `station` | str? | `alchemy_table` / `workbench` / `scribe_table` / `forge` |
| `tool` | obj? | `{type}` — тип предмета-инструмента |
| `requires` | obj | `{attr, skill}` — дополнительные гейты |
| `inputs` | list | `[{type, qty}]` |
| `time_s` | int | длительность крафта (дефолт 1) |
| `xp` | obj | `{profession: N}` — опыт профессии |
| `output` | obj | `{type, qty}` |
| `station_label` | str? | подпись станка в UI |
| `extra` | obj | расширения |

| `id` | Профессия | Станок | Инструмент | Вход | Время | XP | Выход |
|---|---|---|---|---|---|---|---|
| `rec_potion_heal` | alchemist | `alchemy_table` | `flask` | `herb` ×2 | 3 с | 65 | `potion_heal` |
| `rec_staff_oak` | woodcarver | `workbench` | `chisel` | `wood_oak` ×3 | 5 с | 65 | `staff_apprentice` |
| `rec_scroll_frost` | scribe | `scribe_table` | `quill` | `parchment` ×1, `ink` ×1 | 4 с | 65 | `scroll_frost` |

Станок ищется среди объектов локации по `station`; рецепт без станка
крафтится где угодно. Запущенный крафт хранится в `craft_jobs`
(`finishes_at`), результат выдаётся по завершении.

### 1.5 `quests.json` → квесты (1, сид в `quests`)

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `title` | str | ключ и заголовок |
| `giver` | str? | id NPC-выдавателя |
| `goals` | list | `[{type, target?, need, text}]` |
| `reward` | obj | `{xp, items: [{type, qty}], rank_up?}` |
| `reward_label` | str? | готовая подпись награды |
| `extra` | obj | расширения |

Текущий квест `q_wolf_hunt` («Волчья беда»): выдаёт `npc_elder`, цель
`kill` → `m_3` ×1, награда 100 XP в пул + `potion_heal` ×1 +
`rank_up: "skinning"` (ранг профессии без золота). Тип цели в текущем
рантайме — `kill`; `target` — id сущности, `need` — количество.

Состояния: `active` → `turn_in` → `done`; `available` не хранится в БД и
вычисляется по `giver`/прогрессу. Прогресс целей — JSONB
`character_quests.progress`. Награда `reward.xp` идёт в общий пул опыта
(`characters.xp_pool`), а не сразу в уровень.

### 1.6 `locations.json` → локации (1189, сид в `locations`)

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `name` | str | ключ и имя |
| `spawn` | bool | точка старта (дефолт `false`) |
| `flags` | list | `safe` / `pvp` |
| `text` | str | описание |
| `exits` | list | `[{id, label, dir?, to}]` |
| `entities` | list | сущности локации (wire `Entity`) |
| `extra` | obj | расширения |

`ContentExit.to_wire()` отдаёт `Exit` без `to`: клиент не должен сам
телепортироваться по id, переход делает сервер. `entities` в контенте —
это те же wire-сущности (`kind`, `name`, `hp`, `tags`, `extra`), поэтому
контент и снапшот используют одну схему.

Сущности в активной карте:

| `kind` | Кол-во | Смысл |
|---|---:|---|
| `monster` | 6 | боевые мобы и босс |
| `npc` | 49 | диалоги/услуги |
| `object` | 50 | станки, декор, точки взаимодействия |
| `resource` | 4 | добывающие узлы |
| `container` | 1 | сундук |
| `corpse` | 1 | статический труп |
| `player` | 0 | только рантайм |

Из 50 объектов 6 — станки (`alchemy_table` ×2, `workbench` ×2, `forge` ×1,
`scribe_table` ×1), остальные 44 без `station`.

**Тренер.** Ровно один NPC (`npc_elder`) несёт маркер:

```json
"trainer": {
  "professions": ["..."],
  "skills": ["..."],
  "price": {"gold_base": 100, "gold_per_rank": 50, "items": []},
  "price_profession": {"gold_base": 500}
}
```

Маркер — гейт «здесь есть тренер» (`E_NO_TRAINER`) и источник цены
**профессий** (`price_profession`, `gold_price = base + per_rank × rank`).
Цену **навыков** сервер берёт из `dialogs.training` (очки опыта, золото,
предметы) — маркер лишь подтверждает, что NPC вообще тренер (ADR-009).
У `npc_elder` маркер перечисляет 32 навыка (без `dark`/`power_strike`), но
фактически обучаемы только три записи `training`: `sword`, `bow`, `fire`.

**Объекты.** `extra.station` — `alchemy_table`, `workbench`, `scribe_table`,
`forge`. Объект без `station` — декор/точка осмотра. Магазины в
`locations.json` не маркируются: они целиком в `shops.json`.

#### 1.6.1 Монстры

| Поле | Тип | Смысл |
|---|---|---|
| `id`, `name` | str | ключ и имя |
| `hp` | `{cur, max}` | запас здоровья |
| `tags` | list | `aggro`, `boss` и др. |
| `aggro` | bool | атакует при входе |
| `difficulty` | int | влияет на XP убийства |
| `damage` | int? | урон атаки (дефолт 4) |
| `attack_speed` | int? | мс, добавляемые к базовому интервалу |
| `respawn` | `{min_s, max_s}`? | диапазон респавна |
| `drops` | list | строки `def_id` (шанс 1.0) или `{def_id, chance}` |
| `harvest` | obj? | `{level, skin, gut, skin_extra?}` — снятие шкур/потрошение; `skin_extra` — редкие трофеи `[{def_id, chance}]` |
| `boss_ai` | obj? | фазы и способности босса (урон, `self_buff`, `effect`, `summon`) |

Пример обычного моба (`m_3`): 18 HP, `difficulty 3`, `respawn 60–120`,
`harvest {level: 20, skin: true, gut: true}`, дроп `potion_heal` +
`gold_coin`.

`boss_ai`:

```json
"boss_ai": {
  "phases": [
    {"after_s": 0, "name": "Пробуждение", "glyph": "a",
     "damage_mult": 1.0, "interval_mult": 1.0},
    {"after_s": 30, "name": "Ярость", "glyph": "b",
     "damage_mult": 1.5, "interval_mult": 0.85},
    {"after_s": 60, "name": "Разрушение", "glyph": "c",
     "damage_mult": 2.0, "interval_mult": 0.7}
  ],
  "skills": [
    {"id": "flame_breath", "name": "Огненное дыхание", "glyph": "f",
     "cooldown_s": 20, "damage": 25, "cast_s": 2,
     "effect": {"kind": "blind", "duration_s": 4}},
    {"id": "tail_swipe", "name": "Удар хвостом", "glyph": "t",
     "cooldown_s": 12, "damage": 15, "cast_s": 1,
     "effect": {"kind": "stun", "duration_s": 3}},
    {"id": "dragon_fury", "name": "Ярость дракона", "glyph": "r",
     "cooldown_s": 45, "damage": 0, "cast_s": 3,
     "self_buff": {"damage_mult": 1.5, "interval_mult": 0.85,
                   "duration_s": 15}}
  ]
}
```

Фаза выбирается по времени боя (`after_s`), множители применяются к урону
и интервалу атак. Способность босса: `cooldown_s`, `cast_s`, `damage`,
`self_buff`, `effect` (`stun`/`silence`/`blind`/`bleed` — для bleed поле
`value` = урон/с) и/или `summon` (`{mob, count}` — призыв клонов шаблона
с `evt/entity/spawn`). В текущем контенте босс — `m_dragon` (400 HP,
`difficulty 50`, `damage 10`, `attack_speed 3000`, `respawn 300–600`).

Элита `m_white_wolf` («Белый волк») в Чёрном лесу: 90 HP, `difficulty 6`,
`tags: [aggro, elite]`, `boss_ai` из трёх приёмов — **Укус** (урон +
кровотечение `value`/с), **Громкий вой** (`summon` двух `m_3`) и **Рык**
(`self_buff` ×1.5 урона). Снятие шкуры — с уровня 45, редкий трофей
`pelt_white_wolf` — 3 %.

Интервал атаки моба:

```text
base_ms = round(attack_interval_s × 1000) + attack_speed
interval = max(round(base_ms × phase.interval_mult), min_attack_interval_s × 1000)
```

`attack_interval_s` и `min_attack_interval_s` — из `[gameplay.combat]`
(1.0 и 0.5 с). `damage_mult` фазы умножает урон.

#### 1.6.2 Ресурсы, контейнеры и трупы

**Ресурс** — сущность `kind: "resource"` с полями:

| Поле | Смысл |
|---|---|
| `resource` | `def_id` добываемого предмета |
| `max` | максимум зарядов |
| `respawn` | `{min_s, max_s}` |
| `despawn_when_empty` | скрывать узел при нуле зарядов |
| `profession`, `min_rank` | гейт профессии |
| `tool` | тип нужного инструмента |
| `required_level` | M38: требуемый уровень профессии (допуск 25) |
| `gems` | M38/M39: `{chance, chance_mult, mixed_share, mixed[], pure[]}` — бонусные камни; `chance_mult` — множитель шанса конкретной жилы (дефолт 1.0) |

Четыре узла: `r_forest_herb` (`herb`, 1, 30–60, `despawn_when_empty`,
herbalist, ранг 1, `sickle`), `r_forest_tree` (`wood_oak`, 2, 60–120,
woodcutting, `axe`), `r_forest_branches` (`branches`, 5, 30–60,
woodcutting, `axe`), `r_forest_ore` (`ore_copper`, 3, 90–180, mining,
`pickaxe`).

**Контейнер** — `kind: "container"`, `loot: [def_id]`, `respawn`.
`cont_forest_chest` содержит `gold_coin`, `potion_heal`, `herb` и
восстанавливает лут через `respawn` (60–120 с); без `respawn` лут
однократный. Отметки взятия — `loot_taken` (на персонажа); при
респавне сундука они сбрасываются. Админ-оверлей
(`container_removed`/`container_extra`) живёт в памяти.

**Труп** — статический (`cor_9`, `was_player: true`, `loot:
[dagger_knife, potion_heal]`) или динамический, создаваемый при смерти
моба/игрока. Содержит базовый лут и `inner_loot` (результат снятия шкур/
потрошения). TTL — `[world] corpse_ttl_s = 60`; предметы на земле —
`drop_ttl_s = 60`.

### 1.7 `dialogs.json` → диалоги (5 NPC, только память)

| Поле | Тип | Смысл |
|---|---|---|
| `id` | str | id NPC (совпадает с сущностью локации) |
| `root` | str? | стартовая реплика |
| `lines` | list | реплики NPC |
| `training` | list | `[{skill, points, gold, items}]` |

Реплика: `{id, text, when?, quest?, every_s?, replies[]}`. Ответ:
`{id, text, next?, when?, action?}`. `next: null` закрывает диалог;
`action` — `shop` или `train`; `wait_s` в wire — задержка показа текста.
`when` — условия: `gender`, `level {min}`, `attr {name, min}`, `skill`/
`profession {id, min_rank}`, `item {def_id, qty}`. `quest` привязывает
реплику к квесту, `every_s` — кулдаун повторного показа (например,
`l_errand` у `npc_elder` — 1800 с).

`training` — единственный источник цены обучения навыку: `points` —
очки опыта из пула, `gold` — золото, `items` — предметы. Панель
профессий NPC не хранится в диалоге: она собирается из маркера `trainer`
в `locations.json` (`professions`, `price_profession`).

Пример `npc_elder`: `root: "l_hello"`, обучение `sword`/`bow`/`fire`
(10 очков, 100 золота; для `fire` ещё `herb` ×1), ответы ведут в
`l_errand` (квест `q_wolf_hunt`), `l_pack`, `l_female`, `l_veteran`, а
также `action: "shop"` / `"train"`.

### 1.8 `shops.json` → магазины (5 NPC, только память)

| Поле | Тип | Смысл |
|---|---|---|
| `id` | str | id NPC |
| `markup` | float | наценка NPC (дефолт 1.0) |
| `currencies` | list | `[{currency, rate, items: [def_id]}]` |

Пять NPC: `npc_elder` (1.15; золото и `gem_str` по 0.05), `npc_city_elder`
(1.1), `npc_weaponsmith` (1.15), `npc_armorer` (1.12), `npc_alchemist`
(1.05). Списки предметов заданы для каждой валюты отдельно.

Цена:

```text
discount_bp = account.shop_discount_bp + Σ(rank × per_rank × 10000)
discount_bp = clamp(discount_bp, 0, max_discount_bp)
price_gold   = max(floor(gold_value × server_markup × npc.markup
                         × (1 − discount_bp / 10000)), 1)
price        = max(round(price_gold × rate), 1)
```

`server_markup = 1.0`, `max_discount_bp = 5000`; скидка навыка `haggle` —
2% за ранг (`[gameplay.shop.skill_discounts]`). `rate` — единиц валюты за
золото; для `gold_coin` — 1.0. Продажа NPC идёт по `gold_value` без
наценки, если не указано иное.

---

## 2. PostgreSQL

24 игровые таблицы плюс служебная `_sqlx_migrations`; миграции
`0001`–`0028`. Сид контента идемпотентен: при совпадении
`meta.content_version` таблицы не переписываются.

### 2.1 Аккаунты, персонажи, служебное

| Таблица | Колонки | Смысл |
|---|---|---|
| `accounts` | `id`, `login UNIQUE`, `password_hash`, `created_at`, `last_login_at`, `banned_until`, `last_char_id`, `email`, `role`, `shop_discount_bp` | аккаунт; `role ∈ player\|admin\|moderator`; скидка 0…10000 bp |
| `refresh_tokens` | `id`, `account_id FK CASCADE`, `token_hash UNIQUE`, `expires_at`, `revoked_at`, `created_at` | refresh-сессии |
| `characters` | `id TEXT PK`, `account_id FK`, `name`, `profile`, `location_id`, `hp_cur/hp_max`, `mp_cur/mp_max`, `xp_pool`, `state_version`, `created_at`, `deleted_at`, `attrs`, `total_xp`, `gender` | персонаж; уникальное имя среди живых; `deleted_at` — soft-delete |
| `meta` | `key PK`, `value` | `content_version`, `spawn_location` |

`characters.attrs` — JSONB `{str, dex, vit, int}` (без `cha` с 0018),
дефолт 10. `mp_*` — ресурс класса: воин — ярость 0/100, лучник —
энергия 100/100, маг — мана (60 по умолчанию). `state_version` растёт при
выборе персонажа и используется в снапшотах. `total_xp` — накопленный
опыт; уровень и границы (`level`, `xp_in_level`, `xp_to_next`) выводятся
из него и в БД не хранятся.

### 2.2 Инвентарь, навыки, профессии, квесты

| Таблица | Колонки | Смысл |
|---|---|---|
| `item_defs` | `id`, `item_type`, `name`, `description`, `personal`, `rarity`, `upgrade`, `stats`, `requires`, `gold_value`, `qty`, `skill`, `sockets`, `rune_sockets`, `slot`, `hands`, `cooldown_s`, `bonuses` | определения предметов |
| `items` | `uid`, `character_id?`, `def_id FK`, `qty`, `upgrade`, `personal`, `equipped_slot`, `created_at`, `sockets`, `sort_order`, `bonuses` | инстансы; `upgrade` — дельта к `item_defs.upgrade`; `personal` — печать персональности (M39) |
| `loot_taken` | `uid`, `character_id`, `taken_at`; PK `(uid, character_id)` | отметки взятого лута; сбрасываются при респавне сущности |
| `character_skills` | `character_id`, `skill_id`, `xp`, `rank`; PK пара | прогресс навыков |
| `skills` | `id`, `name`, `skill_group`, `description` | справочник навыков |
| `character_professions` | `character_id`, `profession_id`, `xp`, `rank`, `level`; PK пара | прогресс профессий; `level` 0…250, `rank` 1…5 |
| `professions` | `id`, `name`, `prof_type` | справочник профессий |
| `character_quests` | `character_id`, `quest_id`, `state`, `progress`; PK пара | `state ∈ active\|turn_in\|done`; `progress` — JSONB по целям |
| `quests` | `id`, `title`, `giver`, `goals`, `reward`, `reward_label` | справочник квестов; `reward` содержит `xp`/`items`/`rank_up` |
| `char_buffs` | `character_id`, `kind`, `title`, `param`, `value`, `pct`, `stacks`, `expires_at`; PK `(character_id, kind)` | временные баффы/дебаффы, переживают рестарт |

`item_defs.cooldown_s` — `NOT NULL DEFAULT 1.0`; `bonuses` — JSONB
`[{kind,id,value,pct?}]`; `rune_sockets` — `NULL` означает дефолт по типу.
`items.sockets` — JSONB-массив слотов; `sort_order` задаёт порядок в
инвентаре (`ORDER BY sort_order NULLS LAST, created_at, uid`). M28:
`items.container` (`backpack`/`bags`/uid сумки) и `items.slot_index`
задают место; уникальный индекс `items_container_slot_uniq`
`(character_id, container, slot_index)` (только при непустом `slot_index`);
`item_defs.bag_slots` — ёмкость сумки, `item_defs.bag_types` (TEXT[])
— какие типы предметов она принимает (`NULL`/пусто — любые),
`item_defs.bag_kind` — вид слота панели (`general`/`reagent`/`quiver`/
`keyring`; индексы `bags` от `bag_slots` до `bag_slots+2` — спец-слоты,
соответствие проверяет `place`, отказ `E_BAG_SLOT`);
`characters.bags_granted` — флаг
разовой выдачи стартовых сумок. Ёмкость — базовые `inventory.slots` +
сумма `bag_slots` надетых сумок. `items.character_id` допускает `NULL`
(предметы вне инвентаря).

`loot_taken.uid` — `loot:<entity>:<idx>` для базового лута и
`inner:<corpse>:<idx>` для внутренностей; уникальность на персонажа
делает лут однократным до респавна сущности. При респавне контейнера
отметки `loot_taken` для его слотов сбрасываются.

`char_buffs`: загрузка `WHERE expires_at > now()`, применение —
`UPSERT stacks = LEAST(stacks + 1, max_stacks)` (лимит `max_stacks`
приходит из контента, в таблице не хранится) и
`expires_at = now() + duration`; истёкшие удаляются
`DELETE ... RETURNING kind`. `param`/`value`/`pct` применяются к
`DerivedParams`, `title`/`stacks` — для UI. Это отдельный слой от
рантайм-эффектов (`char_effects`, §3).

### 2.3 Мир, крафт, социал и журналы

| Таблица | Колонки | Смысл |
|---|---|---|
| `locations` | `id`, `name`, `flags`, `text`, `exits`, `entities` | снимок локаций из контента; `exits`/`entities` — JSONB |
| `recipes` | `id`, `name`, `profession`, `rank`, `station`, `tool_type`, `requires`, `inputs`, `time_s`, `xp_profession`, `output_type`, `output_qty`, `station_label` | рецепты |
| `craft_jobs` | `id`, `character_id`, `recipe_id`, `qty`, `output_type`, `output_qty`, `finishes_at`, `created_at` | запущенные крафты |
| `character_dialog_lines` | `character_id`, `npc_id`, `line_id`, `shown_at`; PK | показы реплик/кулдауны |
| `chat_history` | `id`, `channel`, `from_char`, `name`, `text`, `sent_at` | кольцевая история чата (200/канал) |
| `guilds` | `id`, `name UNIQUE`, `created_at` | гильдии |
| `guild_members` | `guild_id`, `character_id`, `rank`, `joined_at`; PK | `rank ∈ leader\|officer\|member`; персонаж только в одной гильдии |
| `guild_log` | `id`, `guild_id`, `kind`, `text`, `at` | журнал гильдии |
| `crime_statuses` | `character_id`, `kind`, `expires_at`; PK | активный криминальный статус |
| `crime_log` | `id`, `character_id`, `kind`, `at`, `delta_s`, `total_s` | история преступлений |

Криминальные `kind`: `murderer` (high), `marauder` (medium), `thief`
(medium). Статус — одна строка на вид, срок истекает; лог хранит
изменения срока.

`locations` — не источник истины: она пересобирается из контента. Карта
(`map.rs`) читает именно эту таблицу, поэтому после hot reload кэш карты
инвалидируется.

---

## 3. Рантайм мира (`ld2-server/src/world.rs`)

Рантайм — **in-memory** над контентом и БД. При рестарте он собирается
заново: мобы, трупы, ресурсы, контейнеры и предметы на земле не
персистятся. Персистентны только состояние персонажей, баффы, криминал,
`loot_taken`, кулдауны реплик и `craft_jobs`.

### 3.1 `LiveMob`

| Поле | Смысл |
|---|---|
| `id`, `location_id`, `name` | идентификация |
| `hp`, `hp_max`, `hp_start` | текущее/максимальное/стартовое здоровье |
| `difficulty` | сложность (XP, лут-роллы) |
| `drops` | `[{def_id, chance}]` — уже разобранный контент |
| `tags`, `aggro` | поведение |
| `damage` | урон атаки |
| `next_attack_at`, `attack_interval`, `attack_speed`, `target` | тайминг боя |
| `stun_until`, `root_until` | контроль |
| `bleed_dps/until/next_at/src` | кровотечение |
| `burn_dps/until/next_at/period/src` | горение |
| `respawn` | `(min_s, max_s)` или `None` |
| `harvest` | параметры снятия шкур/потрошения |
| `bonus_loot` | дополнительные `def_id` |
| `boss` | `BossRuntime` |

`entity()` проецирует моба в wire `Entity`: `hp`, `tags`, `aggro`,
`boss {phase, name, glyph, casting}`, `attacking` (id цели),
`next_attack_at`, `attack_interval`. `attack_interval` пересчитывается при
смене фазы и эффектов.

### 3.2 `BossRuntime`

| Поле | Смысл |
|---|---|
| `cfg` | разобранный `boss_ai` |
| `since` | начало боя |
| `phase` | индекс текущей фазы |
| `next_skill_at` | кулдаун следующей способности |
| `skill_i` | ротация способностей |
| `casting` | `{skill, until}` — идёт каст |
| `buff_until`, `buff_damage_mult`, `buff_interval_mult` | self-buff |

Фазы переключаются по `since`; способности — по `cooldown_s`, каст
блокирует обычные атаки на `cast_s`. Эффекты (`stun`/`silence`/`blind`)
кладутся в `char_effects` цели.

### 3.3 `LiveCorpse`

`id`, `location_id`, `name`, `loot` (базовый лут, `Vec<Option<String>>`),
`inner_loot` (внутренности), `born_at`, `harvest`, `skin_done`,
`gut_done`. Труп живёт `corpse_ttl_s = 60`; снятие шкур/потрошение
проверяет `harvest.level`, `skin`/`gut` и выдаёт `inner_loot`. Лут
однократный на персонажа через `loot_taken`.

### 3.4 `ResourceNode`

`id`, `location_id`, `name`, `def_id`, `max`, `current`, `respawn`,
`despawn_when_empty`, `hidden`, `profession`, `min_rank`, `tool`,
`required_level`, `gems`. `entity()` отдаёт `kind: resource`,
`hp {cur: current, max}`; при
`despawn_when_empty` и `current == 0` узел скрывается. Сбор —
`cmd/resource.gather`; после опустошения — респавн по `respawn`.

### 3.5 `GroundItem`

`id`, `location_id`, `name`, `def_id`, `qty`, `upgrade`, `sockets`,
`bonuses`, `display`, `dropped_by`, `expires_at`. Проецируется в
`Entity kind: object` с `extra.ground: true`, `def_id`, `qty`, `upgrade`,
`item` (готовый `ItemSnapshot`). Живёт `drop_ttl_s = 60`. «Stateful»
(с улучшением/сокетами/бонусами) хранится отдельно, чтобы не потерять
инстанс.

### 3.6 Прочие рантайм-структуры

- `char_effects: HashMap<char_id, HashMap<kind, until>>` — `stun`,
  `silence`, `blind`; **не** в БД, сбрасывается при рестарте.
- `container_removed` / `container_extra` — админ-оверлей контейнеров в
  памяти; базовый лут — из `locations.entities`, отметки — `loot_taken`.
- `MapCache` (`map.rs`) — производный кэш карты: `locations` (id, name,
  text, exits), `sizes`, `root`; строится из `locations`, фильтрует
  висячие выходы, корень — `meta.spawn_location`.

### 3.7 Персистентность и тайминги

| Что | Где |
|---|---|
| персонаж, инвентарь, навыки, профессии, квесты | PostgreSQL |
| баффы (`char_buffs`), криминал, кулдауны реплик, `loot_taken` | PostgreSQL |
| мобы, трупы, ресурсы, контейнеры, предметы на земле | память |
| `stun`/`silence`/`blind` | память (`char_effects`) |
| запущенные крафты | PostgreSQL (`craft_jobs.finishes_at`) |

TTL: труп 60 с, предмет на земле 60 с; респавн моба — из контента, иначе
`[world]`; ресурс — из `respawn`; контейнер — из `respawn` (если задан).

---

## 4. Wire-типы (`ld2-protocol`)

Снапшоты и события описаны в [`protocol.md`](./protocol.md) §6–§8. Здесь —
поля, которые важно знать при чтении модели.

### 4.1 Персонаж и прогресс

```rust
CharacterState {
  id, name, attrs, skills, professions,
  hp, mp, xp_pool,
  level?, total_xp?, xp_in_level?, xp_to_next?,
  derived?, profile?, gender,
  equip, inventory, crime, quests, buffs, extra
}
```

- `attrs` — `BTreeMap<String, i64>`; `skills` — `SkillSnapshot` (`rank`,
  `xp`, `xp_to_next`, `group`, `description`); `professions` —
  `ProfessionSnapshot` (`rank`, `level`, `xp`, `xp_to_next`).
- `level`/`total_xp`/`xp_in_level`/`xp_to_next` — производные от
  `characters.total_xp`; `xp_pool` — пул для обучения/крафта.
- `derived` — `DerivedParams` (19 полей): `accuracy`, `damage_min/max`,
  `crit_chance`, `crit_damage`, `armor`, `dodge`, `parry_*`, `block_*`,
  `magic_defense`, `magic_dodge`, `magic_resist`, `magic_dispel`,
  `magic_crit_damage`, `magic_crit_chance`.
- `buffs: Vec<BuffState>` — `{kind, title, stacks, expires_at, extra}`;
  `expires_at` — epoch-секунды (как `CrimeStatus.expires_at`).
- `equip: BTreeMap<String, Option<String>>` — слот → uid предмета.
- `crime: CrimeState {statuses: [CrimeStatus]}`;
  `CrimeStatus {kind, severity, expires_at}`.
- `quests: Vec<QuestState>` — `{id, title?, state, goals, reward_preview?,
  reward_label?, extra}`; `state ∈ available|active|turn_in|done|unknown`.
- `CharacterSummary {id, name, profile?, location, summary?, crime: [String],
  online, extra}` — строка списка персонажей.
- `CrimeLogEntry {kind, at, delta_s, total_s}` — запись журнала нарушений.
- `PartyState`/`GuildState` — партия и гильдия; в снапшоте лежат рядом с
  `char` и `location`.

### 4.2 Предметы

```rust
ItemSnapshot {
  uid, type, name, description?, personal?, rarity, upgrade, stats,
  requires?, sockets?, def_id?, slot?, hands?,
  qty?, gold_value?, bonuses?, extra
}
```

- `type` — `item_type` из определения; `upgrade` — суммарный уровень.
- `description` — текст из контента для карточки/тултипа; нет поля — без описания.
- `personal` — на предмете стоит печать: нельзя передавать, при выбросе
  уничтожается. Печать ставится при первом надевании (для инструментов —
  первом применении); нет поля — обычный.
- `stats` — открытая карта; `requires` — `{attr, level?, profession?}`.
- `sockets: Vec<Socket>` — `{index, rune?: RuneInfo, accepts?: "rune"|"gem"}`;
  `RuneInfo {name, spell, charges, max, damage?, def_id?, glyph?, class?,
  rarity?, detail?}`.
- `bonuses: Vec<ItemBonus>` — `{kind, id, value, pct}`; `qty` — количество
  в стаке; `gold_value` — база цены; `def_id` — id определения.
- `ItemSnapshot.sockets` может отсутствовать, если у предмета нет сокетов.

### 4.3 Мир

```rust
Entity {
  id, kind, name, hp?, tags?, was_player?, aggro?, boss?,
  attacking?, next_attack_at?, attack_interval?, extra
}
```

- `EntityKind`: `player`, `monster`, `npc`, `corpse`, `object`, `resource`,
  `container`, `unknown`.
- `BossState {phase, name, glyph?, casting?}`.
- `LocationSnapshot {id, name, flags, text, exits, entities, extra}`;
  `Exit {id, label, dir?}` — без `to`.

### 4.4 Полезные нагрузки

**`cmd/npc.talk`** (ack):

```json
{
  "dialog": {"id": "npc_elder", "name": "Старейшина"},
  "node": {"id": "l_hello", "text": "...", "quest": null,
           "replies": [{"id": "r_shop", "text": "...", "wait_s": null,
                        "action": "shop"}]},
  "quests": [...],
  "actions": ["quests", "shop", "train"],
  "training": [{"skill": "sword", "points": 10, "gold": 100, "items": []}],
  "professions": [{"id": "alchemist", "gold": 500}]
}
```

`professions` — из маркера `trainer` (в `gold` летит только
`gold_base`; итоговая цена — `base + per_rank × rank`); `training` — из
`dialogs.json`; `actions` может содержать `quests`, `shop`, `train`.
`node.replies[].wait_s` — задержка показа; клиент переводит её в
`wait_until_ms`.

**`cmd/entity.info`** (ack):

```json
{"entity": {...}, "loot": [ItemSnapshot], "inner": [ItemSnapshot],
 "was_player": false, "looted": false}
```

`inner` есть только у динамического трупа (внутренности); у статической
сущности его нет. `looted` — весь лут уже забран этим персонажем.

**`cmd/map.world`**:

```json
{"offset": 0}
```

ack:

```json
{"locations": [{"id": "...", "name": "...", "text": "...",
                "exits": [{"id": "...", "label": "...", "dir": "north"}]}],
 "offset": 48, "total": 1189, "root": "l_village"}
```

Сервер отдаёт чанки по бюджету 48 КБ (`CHUNK_BUDGET`), `offset` — начало
следующего чанка. `root` — `meta.spawn_location`. Команда идемпотентна;
кэш инвалидируется hot reload.

**`evt/state/snapshot`** содержит `char`, `location`, `party`, `guild` и
`state_version`; `char.buffs` — актуальные баффы. События `char/buffs`,
`char/afflict`, `char/level_up`, `mob/attack|phase|skill|cast`,
`harvest/result`, `item/dropped|expired`, `dialog/open`,
`craft/profession` дополняют снапшот.

---

## 5. Клиентский store (`ld2-core`)

Store — производные представления; источники — снапшоты и ack-и.

| Поле / тип | Смысл |
|---|---|
| `snapshot` | последний `CharacterState` + локация |
| `npc_dialog: NpcDialogView?` | открытый диалог |
| `map_locations`, `map_offset`, `map_total`, `map_request`, `map_root` | чанки карты и защита от параллельных запросов |
| `pickup: Value?`, `pickup_dirty` | окно подбора/лута |
| `entity_detail: Value?` | карточка сущности |
| `harvest: HarvestProgress?` | прогресс сбора |
| `stun: StunView?` | оглушение |
| `admin_catalog`, `admin_god`, `admin_target_items` | админ-панель |
| `visited: Vec<VisitedLocation>` | история посещений (лимит 50), не карта |
| `log: VecDeque<LogEntry>` | журнал |

### 5.1 Диалог и магазин

```rust
NpcDialogView {
  id, name, node?, transcript, quests, actions,
  shop?, shop_currency?, training, professions,
  training_open, history_open
}
```

- `node: NpcNode {id, text, quest?, replies}`;
  `replies: [NpcReplyOption {id, text, wait_s?, wait_until_ms?, action?}]`.
- `transcript: [NpcTranscriptLine {me, text}]` — история реплик.
- `training: [NpcTraining {skill, points, gold, items}]`;
  `professions: [NpcProfession {id, gold}]`.
- `NpcShopView {tabs: [NpcShopTab {currency, currency_name, items}]}`;
  `NpcShopItem {def_id, name, rarity, price, qty, item?}`.

`stamp_wait_deadlines` превращает `wait_s` в абсолютные
`wait_until_ms`, чтобы окно не зависело от пауз.

### 5.2 Карта

`map_*` хранят сырые чанки `{id, name, text, exits}`; раскладка считается
в `ld2-client/src/map.rs` (`NodePos`, `MapNode`, `Neighborhood` — BFS по
компасу). `map_request` не даёт отправить второй `map.world`, пока не
пришёл текущий чанк; `map_total`/`map_offset` — прогресс загрузки.

### 5.3 Прочие производные

- `HarvestProgress {started_ms, action, name, entity_id}` — полоса сбора.
- `StunView {title, until_ms, started_ms}` — экран оглушения.
- `UpgradeView {item_uid, item_name, preview?, result?}`,
  `SharpenStone {uid, name, quality}` — улучшение/заточка.
- `LogKind`: `System`, `Location`, `CombatOut`, `CombatIn`, `Magic`,
  `Loot`, `Crime`, `Quest`, `Xp`, `Chat`, `ChatGlobal`, `ChatParty`,
  `ChatGuild`, `ChatWhisper`, `ChatSystem`.

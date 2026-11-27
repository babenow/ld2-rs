# Параметры персонажа — расчёт (M30)

**Документ:** справочник формул **всех** параметров персонажа — атрибуты, ресурсы (HP/MP/ярость/
энергия), регенерация, производные характеристики окна «Параметры», уровень и ранги навыков.
**Статус:** утверждён; разделы, помеченные **M30**, вводятся планом
`PLAN-M30-CHAR-PARAMS` (регенерация здоровья и атрибут «духовность»).
**Связи:** [`skills.md`](./skills.md) §1 (роль атрибутов), §2 (опыт/ранги), §8 (баланс-константы);
[`protocol.md`](./protocol.md) §6 (снапшот `CharacterState`), §11 (аддитивность);
[`data-model.md`](./data-model.md) (`characters.attrs`, `hp_max`/`mp_max`).

> **Правило системы:** всё считает **сервер**; клиент только рендерит присланные значения
> (`protocol.md` §2.4, §10). Формулы ниже — нормативные: изменения баланса = правка конфига
> `server.toml` (секция «Конфиг») и этой страницы, не кода.

---

## 1. Атрибуты (базовые)

Хранятся в `characters.attrs` (JSONB), старт при создании — `10` у каждого.

| ID | Название | Что определяет |
|---|---|---|
| `str` | Сила | физ. урон (`damage_min/max`), контрудар (`parry_*`), требования воинских навыков |
| `dex` | Ловкость | точность, крит, уклон, парирование, скрытность, требования воровских навыков |
| `vit` | Стойкость | **макс. HP (M30)**, магическое сопротивление, выживание |
| `int` | Интеллект | макс. MP, магические производные, требования Арканы |
| `spir` | **Духовность (M30)** | **макс. MP и скорость регена маны** |

- `cha` (харизма) удалена из БД миграцией `0018_attrs_no_cha` — в документах и контенте
  не используется.
- Атрибуты растут сервером (награды квестов/требования рангов — перспектива); в текущей
  сборке их добавляют только **самоцветы** в сокетах надетой экипировки: бонус
  `item_defs.stats = {"attr": "vit", "value": 5}` прибавляется к эффективным attrs
  в снапшоте (`session/chars.rs::character_state`).
- Эффективные attrs = JSONB персонажа + самоцветы снаряжения. Именно они участвуют во всех
  формулах ниже, включая гейты рангов навыков (`max(attrs.values()) ≥ gate.attr`,
  `skills.gates`, `E_REQUIREMENT_ATTR`).

### 1.1 Формула духовности на ману (M30)

```
mp_max = mp_max_base + int × mp_per_int + arcana_rank × mp_per_arcana + spir × mp_per_spir
```

`arcana_rank` — ранг навыка `arcana` (0, если не изучен). Баланс-пример (конфиг
`[gameplay.magic.mana]`): `20 + 10×4 + 0×5 + 10×5 = 110` для персонажа 1-го уровня.
Духовность также ускоряет регенерацию маны — см. §3.2.

---

## 2. Ресурсы

| Ресурс | Профиль | Максимум | Источники изменения |
|---|---|---|---|
| HP (`hp_cur/hp_max`) | все | формула §2.1 (M30) | урон, лечение, **реген (M30)**, level-up, респавн |
| MP (`mp_cur/mp_max`) | маг/охотник | формула §1.1 (M30) | касты рун, **реген (M30)**, зелья |
| Ярость | `warrior` | `resource.rage_max` (100) | прирост за урон, падение вне боя |
| Энергия | `archer` | `resource.energy_max` (100) | пассивный прирост |

`resource_max(profile, res, mp_max)` (`systems/combat.rs`): воин → ярость, лучник → энергия,
иначе → `mp_max`. Рабочий максимум персонажа в снапшоте — `max` ресурса его профиля.

### 2.1 Максимум здоровья (M30)

```
hp_max = hp_max_base + vit × hp_per_vit + level × hp_per_level
```

Конфиг `[gameplay.health]`: `40 + 10×5 + 1×10 = 100` на старте (lvl 1, vit 10) — совпадает
с прежним дефолтом колонки. Растёт со **стойкостью и уровнем**; на level-up прирост
добавляется к `hp_cur` (кламп к новому максимуму) и уходит патчем `evt/char/stats`.

`hp_max`/`mp_max` — колонки `characters`, но служат **кэшем формулы**: сервер пересчитывает
их при создании персонажа, level-up, входе в мир (write-through в `character_state`) и
после вставки/извлечения самоцвета. SQL-путя (`heal_hp` через `LEAST(hp_max, …)`, респавн
`hp_cur = hp_max`) продолжают опираться на колонку.

### 2.2 Расход и восстановление

- **Каст руны** тратит ману **и** заряд руны (`mana.rune_cost`, ADR-002); свитки и умения
  воина ману не тратят (`skills.md` §2.4).
- Лечение предметом: `heal_hp` клампится к `hp_max`, клиенту — патч `evt/char/stats` `{hp}`.
- Ярость воина: `+ceil(дамаг × resource.rage_gain_pct)` за нанесённый урон, вне боя падает
  на `resource.rage_decay_s` ед/с (тик `regen_step`).
- Энергия лучника: `+resource.energy_regen_s` ед/с всегда (тот же тик).

---

## 3. Регенерация (M30)

Обе регенерации крутятся в одном серверном тике `regen_step` (`tick/jobs.rs`), раз в секунду
для каждого онлайн-персонажа; клиент получает результат только патчем `evt/char/stats`.

### 3.1 Здоровье

Конфиг `[gameplay.health]`, модель зеркальна мане:

| Состояние | Шаг за тик |
|---|---|
| вне боя | `ceil(hp_max / regen_out_s)` — полный бар за `regen_out_s` секунд |
| в бою | `ceil(hp_max × regen_in_pct_s / 100)` — проценты максимума в секунду |

Оба шага — минимум 1 ед; лечат только `0 < hp_cur < hp_max` (**мёртвые не регенерируют**),
кламп к `hp_max` выполняет `heal_hp`. «В бою» = персонаж состоит в активном бою
(`World::in_combat`), как и для маны.

### 3.2 Мана

Существующая модель (`[gameplay.magic.mana]`) + множитель духовности:

```
шаг = ceil( max × regen_in_pct_s / 100 )   в бою      (min 1)
шаг = ceil( max / regen_out_s )            вне боя     (min 1)
шаг_eff = ceil( шаг × (1 + spir × spirit_regen_pct / 100) )
```

Баланс-пример: `spirit_regen_pct = 1.0` → персонаж с духовностью 10 восстанавливает ману
на 10 % быстрее и в бою, и вне боя. Ярость/энергия от духовности не зависят.

---

## 4. Производные параметры (окно «Параметры»)

Считает `stats::derive(attrs, skills, equipped, base_damage)` (`ld2-server/src/stats.rs`);
бонусы надетых предметов применяются поверх (ADR-010: плоские прибавки складываются,
проценты складываются и применяются к итогу; баффы — ADR-012, `value × стаков`).
Боевой урон/крит в тике считаются отдельно — см. «Влияние на бой» в доке модуля.

### 4.1 Физические

| Параметр | Формула |
|---|---|
| `damage_min/max` | `оружие_min/max + str/2 + руны_оружия` (M32: навык оружия урон не множит — только сила) |
| `accuracy` | `(75 + dex/2 + оружейная_точность_%).clamp(1..99)` |
| `crit_chance` | `round(5 + dex×0.3 + оружейный_крит_%).clamp(0..100)` |
| `crit_damage` | `50 + dex/5` |
| `armor` | `Σ stats.armor` надетых предметов |
| `dodge` | `round(3 + dex×0.4 + rank(evasion) + rank(armor_light) + оружейный_уклон_%).clamp(0..60)` |
| `parry_chance` | только с двуручным оружием: `(5 + rank(parring) + dex/10).clamp(0..60)`, иначе `0` |
| `parry_min/max` | только с двуручным оружием: `str/4 .. str/2` (урон контрудара — от силы), иначе `0` |
| `block_chance` | `(rank(block) × 2 + (str + dex)/10 + базовый шанс щита).clamp(0..60)` |
| `block_min/max` | `Σ stats.param.block_min/max` надетых предметов (щит: «Щит тысячи истин» — `12..18`) |

Оружейные `%` за ранг (`WEAPON_EFFECTS`, M32 — **точность**, не урон): sword/axe/polearm +2 точности,
bow +2/+0.5 (точность/крит), dagger +1.5/+0.7, twohand +3 точности/−0.3 уклон, unarmed +2/+0.5 уклон —
вклад активируется, когда надето оружие с `extra.skill`. Урон удара даёт только оружие и `str`.

### 4.2 Магические

| Параметр | Формула |
|---|---|
| `magic_defense` | `int + rank(m_armor)×2 + Σ stats.magic` |
| `magic_dodge` | `(int×3/10 + rank(m_evasion)).clamp(0..60)` |
| `magic_resist` | `(vit/2 + rank(m_armor)).clamp(0..60)` |
| `magic_dispel` | `(int/2 + rank(m_dispel)).clamp(0..99)` |
| `magic_crit_damage` | `50 + int/5` |
| `magic_crit_chance` | `(5 + int×3/10 + rank(arcana)/2).clamp(0..100)` |

---

### 4.3 Боевой расчёт (M32)

Авто-атака, активные способности и заклинания считаются в серверном тике:

| Шаг | Формула | Код |
|---|---|---|
| Урон игрока | `roll(damage_min..=damage_max)` из §4.1 (оружие + str/2 + навык + руны) | `systems/combat.rs::roll_damage` |
| Попадание | `chance = (accuracy − dodge_моба)` clamp 5..95 %; нет — промах (`miss` в `evt/combat/hit`) | `hit_chance`, `hit_rolls` |
| Парирование | бросок < `parry` моба → урон вдвое (округление вверх) | `after_parry` |
| Броня моба | плоское снижение: `max(1, dmg − armor)` | `after_armor` |
| Магия | `max(1, round(dmg × (1 − resist/100)))` | `after_resist` |
| Атака моба | `mob.accuracy` против `dodge` персонажа; его `parry_chance` — вдвое; броня персонажа — плоско | `tick/combat.rs::plan_mob_attacks` |

**Статы мобов** — в `content/locations.json` у сущности `monster` (аддитивно,
поле `combat` объектом или плоские ключи рядом с `hp`):

```json
"combat": { "armor": 6, "dodge": 6, "parry": 3, "resist": 6, "accuracy": 81 }
```

Дефолты от сложности `d` (старый контент играет без правок): `armor = 2d`,
`dodge = 3+d`, `parry = d`, `resist = 2d`, `accuracy = 75+2d`. Шанс попадания
клампится 5..95 % (полный промах/неуязвимость невозможны), урон после
снижений — минимум 1. Статы уходят клиенту в `Entity.extra` — в подсказке
списка мобов видно броню/уклон/парирование/сопротивление/точность.

**Случайность:** броски — ГПСЧ мира (`World::roll01`, xorshift64*), зерно
`[gameplay.combat] rng_seed` (`0`/нет — из ОС; e2e-тесты задают фиксированное
для детерминизма).

---

## 5. Уровень и ранги

- **Уровень персонажа** (`[gameplay.level]`, `systems/xp.rs`): опыт перехода
  `level → level+1` = `round(A×level² + B×level + C)`; старт `A=50, B=50, C=0` → 100 XP
  на первый уровень, потолок `max_level = 60`. Опыт копится убийствами и квестами
  (`kill_xp_base + kill_xp_per_difficulty × сложность`, `relevance` — `skills.md` §2.1).
- **Ранг навыка** (`[skills]`): `xp_to_next = round(base_xp × growth^rank)`
  (120 × 1.32^rank, `rank_max = 30`); гейты — `skills.gates` (ранг 10 → атрибут 12,
  20 → 18, 30 → 25), проверка по **максимуму эффективных attrs** — духовность участвует.
- Регенерация и максимумы HP/MP от рангов навыков не зависят (только от атрибутов,
  уровня и конфига).

---

## 6. Конфиг (все ключи формул)

```toml
[gameplay]                     # M36: бездействие персонажа
afk_timeout_s = 300            # 0 — выключено; предупреждение — последние 5 %

[gameplay.health]              # M30: здоровье и его регенерация
hp_max_base = 40               # hp_max = base + vit×hp_per_vit + level×hp_per_level
hp_per_vit = 5.0
hp_per_level = 10.0
regen_out_s = 60               # вне боя: полный бар за N с
regen_in_pct_s = 0.5           # в бою: % макс. HP в секунду

[gameplay.magic.mana]          # мана: максимум (M30) + реген (D4/ADR-002)
mp_max_base = 20               # M30: mp_max = base + int×mp_per_int + arcana×mp_per_arcana + spir×mp_per_spir
mp_per_int = 4.0
mp_per_arcana = 5.0
mp_per_spir = 5.0
spirit_regen_pct = 1.0         # M30: +1 % скорости регена маны за очко духовности
regen_out_s = 45
regen_in_pct_s = 1.0
rune_cost = 10

[gameplay.resource]            # ресурсы классов
rage_max = 100
energy_max = 100
rage_gain_pct = 0.15
rage_decay_s = 5.0
energy_regen_s = 20.0

[gameplay.level]               # кривая уровня
a = 50.0
b = 50.0
c = 0.0
max_level = 60

[gameplay.combat]
base_damage = 6                # боевая база, когда оружие не надето
attack_interval_s = 1.0
rng_seed = 0                   # M32: зерно боевых бросков (0 — случайное)

[gameplay.premium]             # M33: премиум-аккаунт
xp_mult = 1.5                  # +50 % опыта персонажа/навыка за убийство
profession_mult = 1.5          # профессии (крафт/сбор/снятие шкур); множители перемножаются

[skills]                       # ранги навыков (skills.md §8)
base_xp = 120
growth = 1.32
rank_max = 30
gates = { 10 = 12, 20 = 18, 30 = 25 }
```

Образцы: `server.example.toml`, `deploy/server.toml`, локальный `server.toml`
(не в git). Валидация ключей — `config/validate.rs` (fail-fast при старте).

---

## 7. Карта кода

> **Фасад пересчёта (M30):** все параметры считает одна чистая функция
> `stats::recalculate(input) -> Recalc` — она поочерёдно вызывает `max_hp` → `max_mp` →
> `resource_max` → `derive` и возвращает `Recalc { hp_max, mp_max, resource_max, derived }`.
> Фасад ничего не пишет в БД и не шлёт событий: персист (`set_max`) и патчи `evt/char/stats`
> делают вызывающие точки — снапшот, level-up, вставка/извлечение самоцвета, создание
> персонажа. Регенерация фасадом не считается — это тик-дельты `regen_step` (§3).

| Что | Где |
|---|---|
| Фасад пересчёта всех параметров (M30) | `ld2-server/src/stats.rs` (`recalculate`) |
| Производные параметры | `ld2-server/src/stats.rs` (`derive`) |
| Максимумы HP/MP (M30) | `ld2-server/src/stats.rs` (`max_hp`, `max_mp`) |
| Регенерация/ресурсы | `ld2-server/src/systems/combat.rs` (`resource_step`, `rage_gain`), `ld2-server/src/tick/jobs.rs` (`regen_step`) |
| Уровень/опыт | `ld2-server/src/systems/xp.rs` |
| Эффективные attrs, снапшот | `ld2-server/src/session/chars.rs` (`character_state`) |
| Колонки HP/MP, лечение | `ld2-server/src/db/characters.rs` (`heal_hp`, `set_mp`, `set_max` — M30) |
| Конфиг | `ld2-server/src/config/gameplay.rs`, `world.rs`, `validate.rs` |
| Рендер окна | `ld2-client/src/root/sheet/character.rs` («Атрибуты», «Параметры»), `sheet/widgets.rs` (`attr_label`) |

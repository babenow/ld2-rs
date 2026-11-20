//! Мир в памяти (D31, M12.5): боевое состояние, мобы/трупы локаций и
//! реестр соединений для серверной рассылки (в т.ч. событий тика боя).
//!
//! Мобы и динамические трупы **эфемерны**: при рестарте воссоздаются из
//! сида (D26) — их hp не переживает перезапуск (отклонение M12.5).
//! Персонажи, инвентарь и лут статических трупов — в БД (D27).

use std::collections::{HashMap, HashSet};
// R15: bounded Sender<String> (канал 256 — см. transport.rs).
type OutSender = tokio::sync::mpsc::Sender<String>;

use ld2_protocol::types::{Entity, EntityKind, Hp};

/// M20: что возрождается (дизайн M20: `RespawnEntry {entity_id,
/// location_id, kind, at}`; `at = now + random(min..max)`).
#[derive(Debug, Clone, PartialEq)]
pub enum RespawnKind {
    /// Моб: вернуть из шаблона сида; `corpse_id` — старый труп
    /// (удалить вместе с возвратом).
    Mob { corpse_id: Option<String> },
    /// Статический лут: сброс `loot_taken` по префиксу `loot:<entity>:%`.
    Loot,
    /// M20/F20b: ресурс — восстановить `current = max` (жилы/травы).
    Resource,
}

/// M20: запись очереди возрождений (тёплое время мира, не
/// персистентно: рестарт → сид).
#[derive(Debug, Clone, PartialEq)]
pub struct RespawnEntry {
    pub entity_id: String,
    pub location_id: String,
    pub kind: RespawnKind,
    /// Epoch-с, когда возрождать.
    pub at: i64,
}

/// M20: элемент таблицы лута — предмет и шанс выпадения (0..1);
/// простая строка в контенте → `chance = 1.0` (прежнее поведение).
#[derive(Debug, Clone, PartialEq)]
pub struct DropEntry {
    pub def_id: String,
    pub chance: f64,
}

/// Бросок шанса (0..1) — CSPRNG (getrandom); R7: публичный для
/// боевых/заточных бросков (wall-clock-хэш позволял угадывать исход).
pub fn roll_chance(chance: f64) -> bool {
    if chance >= 1.0 {
        return true;
    }
    if chance <= 0.0 {
        return false;
    }
    let mut buf = [0u8; 2];
    if getrandom::fill(&mut buf).is_err() {
        return true; // без RNG лут не теряем (как раньше — всегда)
    }
    (u16::from_be_bytes(buf) as f64) / (u16::MAX as f64) < chance
}

/// M20: случайные секунды в `[min, max]` (респавн-диапазоны;
/// getrandom уже в графе). `max < min` → `min`.
fn rand_range(min: i64, max: i64) -> i64 {
    let min = min.max(0);
    let max = max.max(0);
    if max <= min {
        return min;
    }
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        return min;
    }
    let span = (max - min + 1) as u64;
    min + (u64::from_be_bytes(buf) % span) as i64
}

/// M25: фаза босса — по времени боя (`after_s` от вступления в бой);
/// множители силы/скорости; `glyph` — знак фазы (шрифт рун).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BossPhase {
    pub after_s: i64,
    pub name: String,
    #[serde(default)]
    pub glyph: Option<String>,
    #[serde(default = "boss_mult_one")]
    pub damage_mult: f64,
    #[serde(default = "boss_mult_one")]
    pub interval_mult: f64,
}

/// M25: бафф босса на себя при применении навыка.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BossSelfBuff {
    #[serde(default = "boss_mult_one")]
    pub damage_mult: f64,
    #[serde(default = "boss_mult_one")]
    pub interval_mult: f64,
    pub duration_s: i64,
}

/// M25: навык босса — кулдаун, каст (подготовка) и урон/бафф.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BossSkill {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub glyph: Option<String>,
    pub cooldown_s: i64,
    pub damage: i64,
    /// M25: время применения, с;0 — мгновенно. Во время каста босс
    /// не атакует (подготовка).
    #[serde(default)]
    pub cast_s: i64,
    /// M25: бафф на себя при применении.
    #[serde(default)]
    pub self_buff: Option<BossSelfBuff>,
    /// M25: эффект на игроков при применении (стан/немота/ослепление).
    #[serde(default)]
    pub effect: Option<BossEffect>,
}

/// M25: эффект приёма босса на игроков локации.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BossEffect {
    /// `stun` (ничего нельзя) | `silence` (нет магии) | `blind` (нет атак).
    pub kind: String,
    pub duration_s: i64,
}

/// M25: название эффекта контроля (сервер — истина, RU).
pub fn effect_title(kind: &str) -> String {
    match kind {
        "stun" => "Оглушение".to_string(),
        "silence" => "Немота".to_string(),
        "blind" => "Ослепление".to_string(),
        other => other.to_string(),
    }
}

/// M25: исход такта навыков босса.
#[derive(Debug, Clone)]
pub enum BossSkillTick {
    None,
    /// Навык начал кастоваться (подготовка).
    Started(BossSkill),
    /// Навык применён (мгновенный или завершённый каст).
    Applied(BossSkill),
}

/// M25: активный каст босса.
#[derive(Debug, Clone)]
pub struct BossCast {
    pub skill: BossSkill,
    /// Epoch, мс завершения подготовки.
    pub ends_at: i64,
}

/// M25: конфиг босса из контента (`boss: {phases, skills}`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BossConfig {
    #[serde(default)]
    pub phases: Vec<BossPhase>,
    #[serde(default)]
    pub skills: Vec<BossSkill>,
}

fn boss_mult_one() -> f64 {
    1.0
}

/// M25: рантайм босса — фазы идут от вступления в бой (первая атака),
/// навыки — на кулдауне с ротацией. Респавн берёт чистый сид.
#[derive(Debug, Clone)]
pub struct BossRuntime {
    pub cfg: BossConfig,
    /// Вступление в бой (epoch, мс);0 — ещё не сражался.
    pub since: i64,
    pub phase: usize,
    /// Следующий навык (epoch, мс);0 — готов сразу.
    pub next_skill_at: i64,
    /// Ротация навыков.
    pub skill_i: usize,
    /// M25: активный каст — во время подготовки босс не атакует.
    pub casting: Option<BossCast>,
    /// M25: бафф на себя (множители действуют до buff_until).
    pub buff_until: i64,
    pub buff_damage_mult: f64,
    pub buff_interval_mult: f64,
}

impl BossRuntime {
    pub fn new(cfg: BossConfig) -> Self {
        Self {
            cfg,
            since: 0,
            phase: 0,
            next_skill_at: 0,
            skill_i: 0,
            casting: None,
            buff_until: 0,
            buff_damage_mult: 1.0,
            buff_interval_mult: 1.0,
        }
    }

    /// M25: применить self_buff навыка (множители до duration_s).
    fn apply_self_buff(&mut self, skill: &BossSkill, now_ms: i64) {
        if let Some(buff) = &skill.self_buff {
            self.buff_until = now_ms + buff.duration_s.max(1) * 1000;
            self.buff_damage_mult = buff.damage_mult.max(0.0);
            self.buff_interval_mult = buff.interval_mult.max(0.05);
        }
    }
}

/// Живой моб локации (создаётся из сида, умирает → труп).
#[derive(Debug, Clone)]
pub struct LiveMob {
    pub id: String,
    pub location_id: String,
    pub name: String,
    pub hp: i64,
    pub hp_max: i64,
    /// Сложность 1..10 — XP убийства (skills.md §2.3).
    pub difficulty: i64,
    /// Таблица лута (M20): строки контента = шанс1.0, объекты с
    /// `chance` 0..1 — по шансу; в труп попадают только выпавшие.
    pub drops: Vec<DropEntry>,
    pub tags: Vec<String>,
    pub aggro: bool,
    /// Урон моба (content `damage`, по умолчанию 4).
    pub damage: i64,
    /// Следующая атака моба (epoch, мс).
    pub next_attack_at: i64,
    /// M24: одноразовый «админ-лут» (админ-панель): попадёт в труп
    /// при смерти; респавн создаёт нового моба → пусто (in-memory).
    pub bonus_loot: Vec<String>,
    /// M18/F58d (ADR-012 §5): дебаффы рун на мобе.
    /// Стан (эпоха, мс): моб не атакует, пока не истёк.
    pub stun_until: i64,
    /// Корни (эпоха, мс): обездвиживание — **вступит в силу с
    /// перемещением мобов** (решение заказчика 2026-11-15); сейчас
    /// поле хранится, но на бой не влияет.
    pub root_until: i64,
    /// Кровотечение (DoT): урон в секунду, срок и следующий тик.
    pub bleed_dps: i64,
    pub bleed_until: i64,
    pub bleed_next_at: i64,
    /// Кто наложил кровотечение — XP за добивание от DoT.
    pub bleed_src: Option<String>,
    /// M20: респавн-диапазон (content `respawn {min_s,max_s}` или
    /// дефолт `[world]`); `None` — не возрождается.
    pub respawn: Option<(i64, i64)>,
    /// M20: стартовый HP из сида — восстанавливается при респавне.
    pub hp_start: i64,
    /// Снятие шкур: флаги разделки из контента (`harvest`:
    /// `{level, skin, gut}`) — переходят в труп при убийстве.
    pub harvest: Option<serde_json::Map<String, serde_json::Value>>,
    /// M25: босс — фазы по времени боя и навыки на кулдауне.
    pub boss: Option<BossRuntime>,
    /// M25: интервал атаки, мс (бар клиента;0 — ещё не планировался).
    pub attack_interval: i64,
    /// M25: прибавка к интервалу атаки, мс (content `attack_speed` —
    /// замедление скорости атаки моба).
    pub attack_speed: i64,
    /// M25: кого монстр атакует (id цели) — «атакует вас/имя».
    pub target: Option<String>,
}

impl LiveMob {
    /// Проводной вид §6 (для списков сущностей и снапшотов).
    pub fn entity(&self) -> Entity {
        Entity {
            id: self.id.clone(),
            kind: EntityKind::Monster,
            name: self.name.clone(),
            hp: Some(Hp {
                cur: self.hp,
                max: self.hp_max,
            }),
            tags: if self.tags.is_empty() {
                None
            } else {
                Some(self.tags.clone())
            },
            was_player: None,
            aggro: Some(self.aggro),
            boss: self.boss.as_ref().and_then(|b| {
                let p = b.cfg.phases.get(b.phase)?;
                Some(ld2_protocol::types::BossState {
                    phase: b.phase as u32,
                    name: p.name.clone(),
                    glyph: p.glyph.clone(),
                    casting: b.casting.as_ref().map(|c| c.skill.name.clone()),
                })
            }),
            next_attack_at: (self.next_attack_at > 0).then_some(self.next_attack_at),
            attack_interval: (self.attack_interval > 0).then_some(self.attack_interval),
            attacking: self.target.clone(),
            extra: Default::default(),
        }
    }
}

/// Динамический труп (убитый моб): `loot[i] = Some(def)` — `None` уже взят.
#[derive(Debug, Clone)]
pub struct LiveCorpse {
    pub id: String,
    pub location_id: String,
    pub name: String,
    pub loot: Vec<Option<String>>,
    /// M20: когда труп создан (epoch-с) — TTL-удаление (`corpse_ttl_s`).
    pub born_at: i64,
    /// Снятие шкур: флаги разделки животного (из контента моба);
    /// `None` — разделывать нельзя.
    pub harvest: Option<serde_json::Map<String, serde_json::Value>>,
    /// Уже снята шкура / уже освежено (каждое — один раз).
    pub skin_done: bool,
    pub gut_done: bool,
    /// Лут **освежевания** (мясо/потроха) — отдельный блок «Внутри»
    /// в окне осмотра, не в общем списке.
    pub inner_loot: Vec<Option<String>>,
}

/// Снятие шкур: активная разделка (сервер-джоб; ключ в мире —
/// персонаж, одно действие за раз).
#[derive(Debug, Clone)]
pub struct HarvestJob {
    pub char_id: String,
    pub corpse_id: String,
    /// `skin` | `gut`.
    pub action: String,
    /// Имя животного (без «Труп:») — для журнала клиента.
    pub name: String,
    /// Epoch-с окончания (старт +5 с).
    pub at: i64,
}

/// M20/F20b: объект-ресурс в локации (жила/дерево/трава): `current`
/// убывает по 1 за добычу, при 0 — респавн `[min_s, max_s]`; трава
/// (`despawn_when_empty`) при исчерпании скрывается до респавна.
#[derive(Debug, Clone)]
pub struct ResourceNode {
    pub id: String,
    pub location_id: String,
    pub name: String,
    /// Предмет-выход (content `resource`), 1 за добычу.
    pub def_id: String,
    pub max: i64,
    pub current: i64,
    pub respawn: (i64, i64),
    pub despawn_when_empty: bool,
    /// Скрыт до респавна (трава после исчерпания).
    pub hidden: bool,
    /// Требуемая профессия + ранг (отказ — `E_PROFESSION`).
    pub profession: Option<String>,
    pub min_rank: i32,
    /// Тип инструмента (`item_type` в инвентаре, `E_TOOL_MISSING`).
    pub tool: Option<String>,
}

impl ResourceNode {
    /// Проводной вид §6 (spawn/снапшот): запас — как шкала hp
    /// (`{cur, max}`), клиент рисует бар и патчит через `entity/move`.
    pub fn entity(&self) -> Entity {
        Entity {
            id: self.id.clone(),
            kind: EntityKind::Resource,
            name: self.name.clone(),
            hp: Some(Hp {
                cur: self.current,
                max: self.max,
            }),
            tags: None,
            was_player: None,
            aggro: None,
            boss: None,
            next_attack_at: None,
            attack_interval: None,
            attacking: None,
            extra: Default::default(),
        }
    }
}

/// M20: предмет, лежащий в локации (выброшенный игроком): виден всем,
/// подбираем любым игроком, исчезает по `drop_ttl_s`.
#[derive(Debug, Clone)]
pub struct GroundItem {
    pub id: String,
    pub location_id: String,
    /// Название (для журнала/осмотра без запроса в БД).
    pub name: String,
    pub def_id: String,
    pub qty: i64,
    /// Кто бросил (char_id) — для журнала.
    pub dropped_by: String,
    /// Epoch-секунда исчезновения.
    pub expires_at: i64,
}

impl LiveCorpse {
    /// Проводной вид §6 (счётчик лута — в `evt/corpse/created.loot_count`).
    /// Снятие шкур: флаги `harvest` (+`skin_done`/`gut_done`) — клиент
    /// по ним решает, показывать ли пункты меню.
    pub fn entity(&self) -> Entity {
        let mut extra = serde_json::Map::new();
        if let Some(h) = &self.harvest {
            let mut h = h.clone();
            h.insert("skin_done".into(), serde_json::json!(self.skin_done));
            h.insert("gut_done".into(), serde_json::json!(self.gut_done));
            extra.insert("harvest".into(), serde_json::Value::Object(h));
        }
        Entity {
            id: self.id.clone(),
            kind: EntityKind::Corpse,
            name: self.name.clone(),
            hp: None,
            tags: None,
            was_player: None,
            aggro: None,
            boss: None,
            next_attack_at: None,
            attack_interval: None,
            attacking: None,
            extra,
        }
    }
}

/// Бой персонажа (сессионный: при дисконнекте/смене локации прерывается).
#[derive(Debug, Clone)]
pub struct CombatState {
    pub target_id: String,
    pub location_id: String,
    /// Следующая атака (epoch, мс).
    pub next_attack_at: i64,
}

/// M18/F58d (ADR-012 §5): руна в **экипированном** оружии — параметры
/// авто-приёма для `plan_combat` (синхронный доступ без БД; сборка —
/// `session::refresh_char_runes` на входе/экипировке/инкрустации/касте).
#[derive(Debug, Clone)]
pub struct SocketedRune {
    /// uid предмета-носителя (запись charges в БД из плана).
    pub staff_uid: String,
    pub socket_index: u32,
    pub def_id: String,
    pub spell: String,
    /// Шанс срабатывания при ударе, 0..1 (контент `stats.chance`).
    pub chance: f64,
    /// Урон при срабатывании (контент `stats.damage`).
    pub damage: i64,
    /// Затрата зарядов (деф 1, `stats.cost`).
    pub cost: i64,
    /// Остаток зарядов (кэш синхронизируется с БД при изменениях).
    pub charges: i64,
    /// Бафф персонажа при срабатывании (контент `stats.buff`).
    pub buff: Option<RuneBuff>,
    /// Эффект на цель-моба при срабатывании (контент `stats.effect`):
    /// `stun`/`root`/`bleed` (ADR-012 §5, M18/F58d).
    pub effect: Option<RuneEffect>,
}

/// M18/F58d: эффект руны на моба (контент `stats.effect`).
#[derive(Debug, Clone)]
pub struct RuneEffect {
    /// `stun` | `root` | `bleed`.
    pub kind: String,
    /// Для `bleed` — урон/сек; для stun/root не используется.
    pub value: i64,
    pub duration_s: i64,
}

/// M18/F58d: параметры баффа руны (контент `stats.buff`).
#[derive(Debug, Clone)]
pub struct RuneBuff {
    /// Ключ баффа — def_id руны (стаки разных рун не смешиваются).
    pub kind: String,
    /// Отображаемое название (сервер — истина, §8).
    pub title: String,
    /// Параметр (`accuracy`/`crit_chance`/`armor`/…).
    pub param: String,
    pub value: i64,
    pub pct: bool,
    pub duration_s: i64,
    pub max_stacks: i32,
}

/// Состояние мира, общее для всех соединений.
#[derive(Default)]
pub struct World {
    mobs: HashMap<String, LiveMob>,
    corpses: HashMap<String, LiveCorpse>,
    /// char_id → бой.
    combat: HashMap<String, CombatState>,
    /// char_id → канал текстовых конвертов соединения (рассылка тика §2.4).
    conns: HashMap<String, OutSender>,
    /// char_id → имя (для боевых строк §8).
    char_names: HashMap<String, String>,
    /// R10: char_id → владелец биндинга (peer сессии) — двойная сессия
    /// одного персонажа невозможна, отвязка только своя.
    char_owner: HashMap<String, String>,
    /// M24: «режим бога» — персонажи с неуязвимостью (админ-toggle;
    /// tick::apply_mob_hit не применяет по ним урон вовсе).
    god_chars: HashSet<String>,
    /// M24: надстройки сундуков (entity_id -> слоты ПОСЛЕ базовых
    /// defs; `None` - взято/удалено; индексы стабильны - только
    /// добавление в конец). In-memory до рестарта.
    container_extra: HashMap<String, Vec<Option<(String, i64)>>>,
    /// M24: глобальные «удалено админом» базовые слоты сундуков
    /// ((entity_id, idx); сильнее персонального `loot_taken` - не
    /// возвращается и при респавне).
    container_removed: HashSet<(String, usize)>,
    /// M24: надстройки магазинов NPC (npc_id -> def_id; `None` -
    /// удалено). In-memory до рестарта.
    shop_extra: HashMap<String, Vec<Option<String>>>,
    /// M24: удалённые админом контентные товары: (npc_id, таб, idx).
    shop_removed: HashSet<(String, usize, usize)>,
    /// char_id → текущая локация (мишени агрессивных мобов).
    char_loc: HashMap<String, String>,
    /// (char_id, skill_id) → до какого момента кулдаун (epoch-с, §4/M14).
    ability_cd: HashMap<(String, String), i64>,
    /// M17/F30f: чем бьёт персонаж (экипированное оружие) — синхронный
    /// доступ из plan_combat; обновляется при входе и смене оружия.
    char_hits: HashMap<String, &'static str>,
    /// M17/F30g: период атаки персонажа, мс — из `cooldown_s`
    /// оружия в правой руке; `None` → конфиг `combat.attack_interval_s`.
    char_attack: HashMap<String, i64>,
    /// M18/F58d (ADR-012 §5): руны экипированного оружия — авто-приёмы
    /// в `plan_combat` (как `char_hits`: вход/экипировка/инкрустация).
    char_runes: HashMap<String, Vec<SocketedRune>>,
    corpse_seq: u64,
    /// M20: предметы, лежащие в локациях (выбросы) + TTL-удаление.
    ground_items: HashMap<String, GroundItem>,
    ground_seq: u64,
    /// M20: очередь возрождений (мобы и статический лут) — тёплое
    /// время мира, при рестарте сбрасывается (дизайн M20).
    respawn_queue: Vec<RespawnEntry>,
    /// M20: шаблоны мобов из сида — источник данных для возврата
    /// (живой моб удаляется при смерти).
    mob_seed: HashMap<String, LiveMob>,
    /// M20: респавн-диапазоны сущностей контента (для лута —
    /// сброс `loot:<entity>:%` по расписанию).
    entity_respawn: HashMap<String, (i64, i64)>,
    /// M20/F20b: объекты-ресурсы локаций (`current` — состояние мира).
    resources: HashMap<String, ResourceNode>,
    /// Снятие шкур: активные разделки (ключ — персонаж, одна за раз;
    /// сервер-джоб на5 с).
    harvest_jobs: HashMap<String, HarvestJob>,
    /// M25: дебаффы контроля персонажей (эффекты приёмов босса):
    /// char_id → (kind → until, epoch мс).
    char_effects: HashMap<String, HashMap<String, i64>>,
}

impl World {
    /// M24: включить/выключить неуязвимость персонажа («режим бога»).
    pub fn set_god(&mut self, char_id: &str, enabled: bool) {
        if enabled {
            self.god_chars.insert(char_id.to_string());
        } else {
            self.god_chars.remove(char_id);
        }
    }

    /// M24: есть ли у персонажа неуязвимость (тик, боевые расчёты).
    pub fn is_god(&self, char_id: &str) -> bool {
        self.god_chars.contains(char_id)
    }

    /// M25: наложить эффект контроля (не короче уже действующего);
    /// true — применён.
    pub fn set_char_effect(&mut self, char_id: &str, kind: &str, until_ms: i64) -> bool {
        let entry = self.char_effects.entry(char_id.to_string()).or_default();
        let fresh = entry.get(kind).copied().unwrap_or(0) < until_ms;
        if fresh {
            entry.insert(kind.to_string(), until_ms);
        }
        fresh
    }

    /// M25: срок активного эффекта (просроченный не активен).
    pub fn char_effect_until(&self, char_id: &str, kind: &str, now_ms: i64) -> Option<i64> {
        let until = self.char_effects.get(char_id)?.get(kind).copied()?;
        (until > now_ms).then_some(until)
    }

    /// M25: активен ли эффект контроля сейчас.
    pub fn char_effect_active(&self, char_id: &str, kind: &str, now_ms: i64) -> bool {
        self.char_effect_until(char_id, kind, now_ms).is_some()
    }

    /// M17/F30f: чем бьёт персонаж — творительный из экипированного
    /// оружия; без данных (до входа) — кулаки.
    pub fn char_hits_as(&self, char_id: &str) -> &'static str {
        self.char_hits.get(char_id).copied().unwrap_or("руками")
    }

    /// M17/F30f: обновить «чем бьёт» (вызывается на входе в мир и при
    /// экипировке/снятии оружия).
    pub fn set_char_hits_as(&mut self, char_id: &str, hits_as: &'static str) {
        self.char_hits.insert(char_id.to_string(), hits_as);
    }

    /// M17/F30g: период атаки из `cooldown_s` оружия; `None` — снять
    /// значение (нет оружия → конфиг).
    pub fn set_char_attack_ms(&mut self, char_id: &str, ms: Option<i64>) {
        match ms {
            Some(ms) => {
                self.char_attack.insert(char_id.to_string(), ms);
            }
            None => {
                self.char_attack.remove(char_id);
            }
        }
    }

    /// M17/F30g: текущий период атаки, мс (`None` — из конфига).
    pub fn char_attack_ms(&self, char_id: &str) -> Option<i64> {
        self.char_attack.get(char_id).copied()
    }

    /// M18/F58d: руны экипированного оружия (пусто — не экипировано
    /// или руны без `chance` не участвуют в авто-приёме).
    pub fn char_runes(&self, char_id: &str) -> &[SocketedRune] {
        self.char_runes
            .get(char_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// M18/F58d: заменить кэш рун персонажа (сборка — `refresh_char_runes`).
    pub fn set_char_runes(&mut self, char_id: &str, runes: Vec<SocketedRune>) {
        if runes.is_empty() {
            self.char_runes.remove(char_id);
        } else {
            self.char_runes.insert(char_id.to_string(), runes);
        }
    }

    /// M18/F58d: расход зарядов в кэше (план авто-приёма; БД — в
    /// `tick_once` по `Plan::rune_work`).
    pub fn spend_char_rune(&mut self, char_id: &str, socket_index: u32, charges: i64) {
        if let Some(runes) = self.char_runes.get_mut(char_id) {
            if let Some(rune) = runes.iter_mut().find(|r| r.socket_index == socket_index) {
                rune.charges = charges;
            }
        }
    }

    /// M18/F58d (ADR-012 §5): наложить эффект руны на моба —
    /// `stun`/`root`/`bleed` (контент `stats.effect`). `root` — поле
    /// под будущее перемещение мобов; `bleed` тикает в `plan_combat`.
    pub fn apply_rune_effect(
        &mut self,
        mob_id: &str,
        kind: &str,
        value: i64,
        duration_s: i64,
        src_char_id: &str,
        now_ms: i64,
    ) {
        let Some(mob) = self.mobs.get_mut(mob_id) else {
            return;
        };
        let until = now_ms + duration_s.max(0) * 1000;
        match kind {
            "stun" => mob.stun_until = until,
            "root" => mob.root_until = until,
            "bleed" => {
                // Продление активного кровотечения НЕ сбрасывает таймер
                // тика (иначе частые удары не дают DoT дотикнуть).
                if mob.bleed_dps == 0 || mob.bleed_until <= now_ms {
                    mob.bleed_dps = value.max(1);
                    mob.bleed_next_at = now_ms + 1000; // первый тик через секунду
                    mob.bleed_src = Some(src_char_id.to_string());
                }
                mob.bleed_until = until;
            }
            _ => {}
        }
    }

    /// M18/F58d: стан моба активен на `now_ms` (атаки пропускаются).
    pub fn mob_stunned(&self, mob_id: &str, now_ms: i64) -> bool {
        self.mobs.get(mob_id).is_some_and(|m| m.stun_until > now_ms)
    }

    /// M18/F58d: мобы с активным кровотечением, у которых на `now_ms`
    /// пора тикнуть → `(id, dps, next_at, src)`.
    pub fn bleeding_mobs(&self, now_ms: i64) -> Vec<(String, i64, i64, Option<String>)> {
        self.mobs
            .values()
            .filter(|m| m.bleed_dps > 0 && m.bleed_until >= now_ms && m.bleed_next_at <= now_ms)
            .map(|m| {
                (
                    m.id.clone(),
                    m.bleed_dps,
                    m.bleed_next_at,
                    m.bleed_src.clone(),
                )
            })
            .collect()
    }

    /// M18/F58d: продвинуть тик кровоточащего моба (следующий через 1 с).
    pub fn advance_bleed_tick(&mut self, mob_id: &str, now_ms: i64) {
        if let Some(mob) = self.mobs.get_mut(mob_id) {
            mob.bleed_next_at = now_ms + 1000;
            if mob.bleed_until <= now_ms {
                mob.bleed_dps = 0; // срок истёк — чистим
            }
        }
    }

    /// M18/F58d: локация моба (для рассылки событий DoT) и его имя.
    pub fn mob_loc_name(&self, mob_id: &str) -> Option<(String, String)> {
        self.mobs
            .get(mob_id)
            .map(|m| (m.location_id.clone(), m.name.clone()))
    }

    /// Загрузить живых мобов из сида (вызывается после content-сида).
    /// `default_respawn` — дефолтный диапазон `[world]` для мобов
    /// без `respawn` в контенте (M20, решение 5).
    pub fn seed_mobs(
        &mut self,
        rows: &[(String, serde_json::Value)],
        default_respawn: Option<(i64, i64)>,
    ) {
        for (location_id, entities) in rows {
            let entities: Vec<Entity> =
                serde_json::from_value(entities.clone()).unwrap_or_default();
            for e in entities {
                let extra = &e.extra;
                // M20: `respawn {min_s,max_s}` у любой сущности
                // (мобам — дефолт [world]); отрицательные → 0,
                // min/max приводятся к порядку.
                let mut respawn = extra
                    .get("respawn")
                    .and_then(|v| v.as_object())
                    .and_then(|o| {
                        let min = o.get("min_s")?.as_i64()?.max(0);
                        let max = o.get("max_s")?.as_i64()?.max(0);
                        Some(if min <= max { (min, max) } else { (max, min) })
                    });
                if respawn.is_none() && e.kind == EntityKind::Monster {
                    respawn = default_respawn;
                }
                if let Some(range) = respawn {
                    self.entity_respawn.insert(e.id.clone(), range);
                }
                // M20/F20b: объект-ресурс — параметры из extra.
                if e.kind == EntityKind::Resource {
                    if let Some(def_id) = extra.get("resource").and_then(|v| v.as_str()) {
                        let max = extra
                            .get("max")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(1)
                            .max(1);
                        self.resources.insert(
                            e.id.clone(),
                            ResourceNode {
                                id: e.id.clone(),
                                location_id: location_id.clone(),
                                name: e.name.clone(),
                                def_id: def_id.to_string(),
                                max,
                                current: max,
                                respawn: respawn.unwrap_or((60, 120)),
                                despawn_when_empty: extra
                                    .get("despawn_when_empty")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false),
                                hidden: false,
                                profession: extra
                                    .get("profession")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                min_rank: extra
                                    .get("min_rank")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(1)
                                    .clamp(1, 30) as i32,
                                tool: extra.get("tool").and_then(|v| v.as_str()).map(String::from),
                            },
                        );
                    }
                    continue;
                }
                if e.kind != EntityKind::Monster {
                    continue;
                }
                let difficulty = extra
                    .get("difficulty")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1)
                    .clamp(1, 10);
                // M20: drops — строки (шанс1.0) или объекты
                // {def_id|id, chance 0..1} (таблица лута).
                let drops: Vec<DropEntry> = extra
                    .get("drops")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| {
                                if let Some(def_id) = v.as_str() {
                                    return Some(DropEntry {
                                        def_id: def_id.to_string(),
                                        chance: 1.0,
                                    });
                                }
                                let obj = v.as_object()?;
                                let def_id = obj
                                    .get("def_id")
                                    .or_else(|| obj.get("id"))
                                    .and_then(|x| x.as_str())?;
                                let chance = obj
                                    .get("chance")
                                    .and_then(|x| x.as_f64())
                                    .unwrap_or(1.0)
                                    .clamp(0.0, 1.0);
                                Some(DropEntry {
                                    def_id: def_id.to_string(),
                                    chance,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let damage = extra.get("damage").and_then(|v| v.as_i64()).unwrap_or(4);
                let hp_start = e.hp.map(|hp| hp.cur).unwrap_or(1);
                let mob = LiveMob {
                    id: e.id.clone(),
                    location_id: location_id.clone(),
                    name: e.name.clone(),
                    hp: hp_start,
                    hp_max: e.hp.map(|hp| hp.max).unwrap_or(1),
                    difficulty,
                    drops,
                    tags: e.tags.clone().unwrap_or_default(),
                    aggro: e.aggro.unwrap_or(false),
                    damage,
                    next_attack_at: 0,
                    stun_until: 0,
                    root_until: 0,
                    bleed_dps: 0,
                    bleed_until: 0,
                    bleed_next_at: 0,
                    bleed_src: None,
                    respawn,
                    hp_start,
                    harvest: extra.get("harvest").and_then(|v| v.as_object()).cloned(),
                    bonus_loot: Vec::new(),
                    // M25: босс — конфиг фаз/навыков из контента
                    // (`boss_ai`: ключ `boss` занят display-полем Entity).
                    boss: extra
                        .get("boss_ai")
                        .and_then(|v| serde_json::from_value::<BossConfig>(v.clone()).ok())
                        .map(BossRuntime::new),
                    attack_interval: 0,
                    attack_speed: extra
                        .get("attack_speed")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    target: None,
                };
                // M20: шаблон сида — источник данных при респавне.
                self.mob_seed.insert(e.id.clone(), mob.clone());
                self.mobs.insert(e.id.clone(), mob);
            }
        }
    }

    /// Регистрация соединения персонажа (рассылка событий тика §2.4).
    /// R10: персонаж, уже привязанный к другой сессии (peer), — отказ:
    /// две WS-сессии не должны действовать одним персонажем (отключение
    /// старой раньше отвязывало мир у новой). Повторный bind той же
    /// сессии — ок (перевыбор персонажа).
    pub fn bind_char(&mut self, char_id: &str, name: &str, tx: OutSender, owner: &str) -> bool {
        if self.char_owner.get(char_id).is_some_and(|cur| cur != owner) {
            return false;
        }
        self.conns.insert(char_id.to_string(), tx);
        self.char_names
            .insert(char_id.to_string(), name.to_string());
        self.char_owner
            .insert(char_id.to_string(), owner.to_string());
        true
    }

    /// R10: отвязка — только своего биндинга (умершая старая сессия
    /// позже новой не отвязывает мир у новой). `true` — отвязано
    /// (тогда соединение чистит и социум).
    pub fn unbind_char(&mut self, char_id: &str, owner: &str) -> bool {
        if self.char_owner.get(char_id).map(String::as_str) != Some(owner) {
            return false;
        }
        self.conns.remove(char_id);
        self.char_names.remove(char_id);
        self.char_loc.remove(char_id);
        self.combat.remove(char_id);
        self.ability_cd.retain(|(c, _), _| c != char_id);
        self.char_owner.remove(char_id);
        true
    }

    /// Позиция персонажа в мире (мишени мобов, бой по локации).
    pub fn set_char_location(&mut self, char_id: &str, location_id: &str) {
        self.char_loc
            .insert(char_id.to_string(), location_id.to_string());
    }

    /// Обратный поиск: имя персонажа → char_id (whisper, ФР-13).
    pub fn char_id_by_name(&self, name: &str) -> Option<String> {
        self.char_names
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|(id, _)| id.clone())
    }

    /// Локация персонажа (проверка сделки «в одной локации», ФР-15).
    pub fn char_location(&self, char_id: &str) -> Option<String> {
        self.char_loc.get(char_id).cloned()
    }

    pub fn char_name(&self, char_id: &str) -> Option<String> {
        self.char_names.get(char_id).cloned()
    }

    /// Локации, где есть персонажи (шаг агрессии мобов).
    pub fn locations_with_players(&self) -> Vec<String> {
        let mut locs: Vec<String> = self.char_loc.values().cloned().collect();
        locs.sort();
        locs.dedup();
        locs
    }

    /// Персонажи, привязанные к миру (онлайн — реген маны, M12.5b).
    pub fn online_chars(&self) -> Vec<String> {
        let mut chars: Vec<String> = self.char_names.keys().cloned().collect();
        chars.sort();
        chars
    }

    /// Персонажи в локации (жертвы агрессии).
    /// M24: очистить все слоты трупа (общий лут и «Внутри») →
    /// обновлённый труп для broadcast.
    pub fn corpse_clear(&mut self, corpse_id: &str) -> Option<LiveCorpse> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        for slot in corpse.loot.iter_mut().chain(corpse.inner_loot.iter_mut()) {
            *slot = None;
        }
        Some(corpse.clone())
    }

    /// M24: выбросить всю надстройку сундука (база — tombstone'ами).
    pub fn container_clear(&mut self, entity_id: &str) {
        self.container_extra.remove(entity_id);
    }

    /// M24: выбросить всю надстройку магазина (контент — tombstone'ами).
    pub fn shop_clear(&mut self, npc_id: &str) {
        self.shop_extra.remove(npc_id);
    }

    /// M24: очистить весь лут живого монстра (восстановится при
    /// респавне из шаблона).
    pub fn mob_drops_clear(&mut self, mob_id: &str) -> bool {
        match self.mobs.get_mut(mob_id) {
            Some(mob) => {
                mob.drops.clear();
                true
            }
            None => false,
        }
    }

    /// M24: админ-лут монстра (одноразовый, в труп при смерти).
    pub fn mob_bonus_add(&mut self, mob_id: &str, def_id: &str) -> bool {
        match self.mobs.get_mut(mob_id) {
            Some(mob) => {
                mob.bonus_loot.push(def_id.to_string());
                true
            }
            None => false,
        }
    }

    /// M24: убрать `count` позиций админ-лута монстра с `idx`.
    pub fn mob_bonus_remove_at(&mut self, mob_id: &str, idx: usize, count: usize) -> bool {
        match self.mobs.get_mut(mob_id) {
            Some(mob) if idx < mob.bonus_loot.len() => {
                let end = (idx + count).min(mob.bonus_loot.len());
                mob.bonus_loot.drain(idx..end);
                true
            }
            _ => false,
        }
    }

    /// M24: текущий админ-лут монстра (для admin.target.info).
    pub fn mob_bonus(&self, mob_id: &str) -> Vec<String> {
        self.mobs
            .get(mob_id)
            .map(|mob| mob.bonus_loot.clone())
            .unwrap_or_default()
    }

    /// M24: инъекция в общий лут трупа → обновлённый труп (для
    /// `evt/corpse/created`); `None` — трупа нет.
    pub fn corpse_loot_add(&mut self, corpse_id: &str, def_id: &str) -> Option<LiveCorpse> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        corpse.loot.push(Some(def_id.to_string()));
        Some(corpse.clone())
    }

    /// M24: убрать `count` слотов трупа начиная с `idx` (общий лут
    /// либо «Внутри» при `inner`) → обновлённый труп; `None` —
    /// трупа/слотов нет.
    pub fn corpse_slot_remove_at(
        &mut self,
        corpse_id: &str,
        inner: bool,
        idx: usize,
        count: usize,
    ) -> Option<LiveCorpse> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let slots = if inner {
            &mut corpse.inner_loot
        } else {
            &mut corpse.loot
        };
        if idx >= slots.len() {
            return None;
        }
        let end = (idx + count).min(slots.len());
        for slot in slots.iter_mut().take(end).skip(idx) {
            *slot = None;
        }
        Some(corpse.clone())
    }

    // ── M24: надстройки сундуков и магазинов (in-memory) ──────────────────

    /// Слоты надстройки сундука (позиции после базовых defs).
    pub fn container_extra(&self, entity_id: &str) -> Vec<Option<(String, i64)>> {
        self.container_extra
            .get(entity_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Добавить в надстройку сундука **один стак** с указанным
    /// количеством (указание заказчика): складывается с уже лежащим
    /// таким же дефом, иначе — одна новая позиция.
    pub fn container_extra_add(&mut self, entity_id: &str, def_id: &str, qty: i64) {
        let slots = self
            .container_extra
            .entry(entity_id.to_string())
            .or_default();
        if let Some(slot) = slots.iter_mut().flatten().find(|slot| slot.0 == def_id) {
            slot.1 = (slot.1 + qty).min(i64::from(i32::MAX));
            return;
        }
        slots.push(Some((def_id.to_string(), qty)));
    }

    /// Слот надстройки сундука: (def_id, количество).
    pub fn container_extra_slot(&self, entity_id: &str, idx: usize) -> Option<(String, i64)> {
        self.container_extra.get(entity_id)?.get(idx)?.clone()
    }

    /// Занять весь слот надстройки (взятие игроком/удаление целиком).
    pub fn container_extra_clear(&mut self, entity_id: &str, idx: usize) -> bool {
        match self.container_extra.get_mut(entity_id) {
            Some(slots) if idx < slots.len() && slots[idx].is_some() => {
                slots[idx] = None;
                true
            }
            _ => false,
        }
    }

    /// Убрать `count` единиц из слота надстройки (частичное удаление
    /// админом): остаток < count → слот освобождается.
    pub fn container_extra_reduce(&mut self, entity_id: &str, idx: usize, count: i64) -> bool {
        match self.container_extra.get_mut(entity_id) {
            Some(slots) if idx < slots.len() => match slots[idx].as_mut() {
                Some((_, qty)) if count >= *qty => {
                    slots[idx] = None;
                    true
                }
                Some((_, qty)) => {
                    *qty -= count;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Базовый слот сундука удалён админом (tombstone)?
    pub fn container_base_removed(&self, entity_id: &str, idx: usize) -> bool {
        self.container_removed
            .contains(&(entity_id.to_string(), idx))
    }

    /// Пометить базовый слот сундука удалённым (глобально).
    pub fn container_base_remove(&mut self, entity_id: &str, idx: usize) {
        self.container_removed.insert((entity_id.to_string(), idx));
    }

    /// Позиции надстройки магазина (`None` - удалено).
    pub fn shop_extra(&self, npc_id: &str) -> Vec<Option<String>> {
        self.shop_extra.get(npc_id).cloned().unwrap_or_default()
    }

    /// Добавить позицию в надстройку магазина — **одна позиция на
    /// предмет** (магазин не лимитирован, копии не нужны).
    pub fn shop_extra_add(&mut self, npc_id: &str, def_id: &str) {
        let slots = self.shop_extra.entry(npc_id.to_string()).or_default();
        if slots.iter().flatten().any(|d| d == def_id) {
            return;
        }
        slots.push(Some(def_id.to_string()));
    }

    /// Удалить `count` позиций надстройки магазина с `idx`.
    pub fn shop_extra_take_at(&mut self, npc_id: &str, idx: usize, count: usize) -> bool {
        match self.shop_extra.get_mut(npc_id) {
            Some(slots) if idx < slots.len() => {
                let end = (idx + count).min(slots.len());
                for slot in slots.iter_mut().take(end).skip(idx) {
                    *slot = None;
                }
                true
            }
            _ => false,
        }
    }

    /// Контентный товар магазина удалён админом?
    pub fn shop_content_removed(&self, npc_id: &str, tab: usize, idx: usize) -> bool {
        self.shop_removed.contains(&(npc_id.to_string(), tab, idx))
    }

    /// Пометить контентный товар магазина удалённым.
    pub fn shop_content_remove(&mut self, npc_id: &str, tab: usize, idx: usize) {
        self.shop_removed.insert((npc_id.to_string(), tab, idx));
    }

    pub fn chars_in_location(&self, location_id: &str) -> Vec<String> {
        let mut chars: Vec<String> = self
            .char_loc
            .iter()
            .filter(|(_, loc)| loc.as_str() == location_id)
            .map(|(id, _)| id.clone())
            .collect();
        chars.sort();
        chars
    }

    /// Список боёв персонажей: (char, цель, локация, следующая атака).
    pub fn combat_list(&self) -> Vec<(String, String, String, i64)> {
        self.combat
            .iter()
            .map(|(c, s)| {
                (
                    c.clone(),
                    s.target_id.clone(),
                    s.location_id.clone(),
                    s.next_attack_at,
                )
            })
            .collect()
    }

    pub fn set_next_attack(&mut self, char_id: &str, at_ms: i64) {
        if let Some(combat) = self.combat.get_mut(char_id) {
            combat.next_attack_at = at_ms;
        }
    }

    /// Кулдаун активной способности: `Some(остаток_с)` (M14, §4).
    pub fn ability_cooldown_left(&self, char_id: &str, skill: &str, now_s: i64) -> Option<i64> {
        let until = self
            .ability_cd
            .get(&(char_id.to_string(), skill.to_string()))?;
        Some(*until - now_s).filter(|left| *left > 0)
    }

    /// Записать кулдаун активной способности (until = now + cooldown_s).
    pub fn set_ability_cooldown(&mut self, char_id: &str, skill: &str, until_s: i64) {
        self.ability_cd
            .insert((char_id.to_string(), skill.to_string()), until_s);
    }

    /// Момент следующей атаки моба (агрессия по тику).
    pub fn mob_next_attack(&self, mob_id: &str) -> Option<i64> {
        self.mobs.get(mob_id).map(|m| m.next_attack_at)
    }

    /// M25: такт босса — вступление в бой (первая атака) и переход
    /// фазы по времени боя; `(фаза, изменилась)` — события клиенту.
    pub fn boss_tick(&mut self, mob_id: &str, now_ms: i64) -> Option<(usize, bool)> {
        let mob = self.mobs.get_mut(mob_id)?;
        let boss = mob.boss.as_mut()?;
        if boss.since == 0 {
            boss.since = now_ms;
        }
        let fight_ms = (now_ms - boss.since).max(0);
        let target = boss
            .cfg
            .phases
            .iter()
            .rposition(|p| fight_ms >= p.after_s.saturating_mul(1000))
            .unwrap_or(0);
        let changed = target != boss.phase;
        boss.phase = target;
        Some((target, changed))
    }

    /// M25: множители текущей фазы босса (урон, интервал); не босс —
    /// единицы (интервал не опуска ниже пола — см. systems).
    pub fn boss_phase_mult(&self, mob_id: &str, now_ms: i64) -> (f64, f64) {
        let Some(boss) = self.mobs.get(mob_id).and_then(|m| m.boss.as_ref()) else {
            return (1.0, 1.0);
        };
        let (mut damage, mut interval) = match boss.cfg.phases.get(boss.phase) {
            Some(p) => (p.damage_mult.max(0.0), p.interval_mult.max(0.05)),
            None => (1.0, 1.0),
        };
        // M25: бафф босса на себя (self_buff) — множит фазу.
        if now_ms < boss.buff_until {
            damage *= boss.buff_damage_mult;
            interval *= boss.buff_interval_mult;
        }
        (damage, interval)
    }

    /// M25: такт навыков босса — каст (подготовка, босс не атакует)
    /// либо применение; ротация навыков, self_buff применяется при
    /// выданном умении. `(исход, идёт каст)`.
    pub fn boss_skill_tick(&mut self, mob_id: &str, now_ms: i64) -> (BossSkillTick, bool) {
        let Some(mob) = self.mobs.get_mut(mob_id) else {
            return (BossSkillTick::None, false);
        };
        let Some(boss) = mob.boss.as_mut() else {
            return (BossSkillTick::None, false);
        };
        // Завершение каста → применение (кулдаун от конца подготовки).
        if let Some(cast) = boss.casting.clone() {
            if now_ms >= cast.ends_at {
                boss.casting = None;
                boss.next_skill_at = now_ms + cast.skill.cooldown_s.max(1) * 1000;
                boss.apply_self_buff(&cast.skill, now_ms);
                return (BossSkillTick::Applied(cast.skill), false);
            }
            return (BossSkillTick::None, true);
        }
        if boss.cfg.skills.is_empty() || now_ms < boss.next_skill_at {
            return (BossSkillTick::None, false);
        }
        let skill = boss.cfg.skills[boss.skill_i % boss.cfg.skills.len()].clone();
        boss.skill_i += 1;
        if skill.cast_s <= 0 {
            // Мгновенный навык — применяем сразу.
            boss.next_skill_at = now_ms + skill.cooldown_s.max(1) * 1000;
            boss.apply_self_buff(&skill, now_ms);
            return (BossSkillTick::Applied(skill), false);
        }
        // Каст: во время подготовки босс не атакует.
        let ends_at = now_ms + skill.cast_s.max(1) * 1000;
        boss.next_skill_at = ends_at + skill.cooldown_s.max(1) * 1000;
        boss.casting = Some(BossCast {
            skill: skill.clone(),
            ends_at,
        });
        (BossSkillTick::Started(skill), true)
    }

    /// M25: состояние босса для события смены фазы (имя/глиф фазы).
    pub fn boss_state(&self, mob_id: &str) -> Option<ld2_protocol::types::BossState> {
        let boss = self.mobs.get(mob_id)?.boss.as_ref()?;
        let p = boss.cfg.phases.get(boss.phase)?;
        Some(ld2_protocol::types::BossState {
            phase: boss.phase as u32,
            name: p.name.clone(),
            glyph: p.glyph.clone(),
            casting: boss.casting.as_ref().map(|c| c.skill.name.clone()),
        })
    }

    /// M25: интервал атаки моба для бара клиента (запоминается при
    /// планировании атаки).
    /// M25: прибавка к интервалу атаки из контента (замедление).
    pub fn mob_attack_speed(&self, mob_id: &str) -> i64 {
        self.mobs
            .get(mob_id)
            .map(|m| m.attack_speed.max(0))
            .unwrap_or(0)
    }

    /// M25: запомнить цель атаки моба (для списка «атакует …»).
    pub fn set_mob_target(&mut self, mob_id: &str, target: &str) {
        if let Some(mob) = self.mobs.get_mut(mob_id) {
            mob.target = Some(target.to_string());
        }
    }

    pub fn set_mob_attack_interval(&mut self, mob_id: &str, ms: i64) {
        if let Some(mob) = self.mobs.get_mut(mob_id) {
            mob.attack_interval = ms;
        }
    }

    pub fn set_mob_next_attack(&mut self, mob_id: &str, at_ms: i64) {
        if let Some(mob) = self.mobs.get_mut(mob_id) {
            mob.next_attack_at = at_ms;
        }
    }

    pub fn mob_damage(&self, mob_id: &str) -> i64 {
        self.mobs.get(mob_id).map(|m| m.damage).unwrap_or(0)
    }

    pub fn mob_max_hp(&self, mob_id: &str) -> i64 {
        self.mobs.get(mob_id).map(|m| m.hp_max).unwrap_or(0)
    }

    /// Количество живых мобов (лог старта).
    pub fn mob_count(&self) -> usize {
        self.mobs.len()
    }

    /// Труп по id (для evt/corpse/created после kill_mob).
    pub fn corpse(&self, corpse_id: &str) -> Option<&LiveCorpse> {
        self.corpses.get(corpse_id)
    }

    /// Прервать бой персонажа (смена локации, стоп, смерть).
    pub fn end_combat(&mut self, char_id: &str) -> bool {
        self.combat.remove(char_id).is_some()
    }

    pub fn in_combat(&self, char_id: &str) -> bool {
        self.combat.contains_key(char_id)
    }

    /// Привязан ли персонаж к миру (есть канал соединения) — онлайн.
    pub fn is_bound(&self, char_id: &str) -> bool {
        self.conns.contains_key(char_id)
    }

    /// Отправить конверт соединению персонажа (best-effort).
    pub fn send_to_char(&self, char_id: &str, text: String) {
        if let Some(tx) = self.conns.get(char_id) {
            // R15: bounded-канал — полный буфер = медленный клиент;
            // рассылка снимается (память ограничена), команды через
            // sink работают; resync восстановит состояние.
            let _ = tx.try_send(text);
        }
    }

    /// Начать/сменить бой с целью в текущей локации; `None` — цели нет
    /// (моб не найден/мёртв/чужая локация) → `E_TARGET_GONE`.
    pub fn start_combat(
        &mut self,
        char_id: &str,
        location_id: &str,
        target_id: &str,
        now_ms: i64,
    ) -> bool {
        let alive = self
            .mobs
            .get(target_id)
            .is_some_and(|m| m.location_id == location_id);
        if !alive {
            return false;
        }
        self.combat.insert(
            char_id.to_string(),
            CombatState {
                target_id: target_id.to_string(),
                location_id: location_id.to_string(),
                next_attack_at: now_ms,
            },
        );
        true
    }

    /// Мёртвые мобы текущей локации уже не отдаются; снапшот §6 = статика
    /// (кроме мобов — они живут здесь) + живые мобы + динамические трупы.
    pub fn build_location(
        &self,
        mut snapshot: ld2_protocol::types::LocationSnapshot,
    ) -> ld2_protocol::types::LocationSnapshot {
        use ld2_protocol::types::EntityKind::*;
        // Мобы и ресурсы живут в мире (content-строка — только сид):
        // снимок не должен дублировать их и показывать убитых/скрытых.
        snapshot
            .entities
            .retain(|e| !matches!(e.kind, Monster | Resource));
        let loc_id = snapshot.id.clone();
        let mut mobs: Vec<&LiveMob> = self
            .mobs
            .values()
            .filter(|m| m.location_id == loc_id)
            .collect();
        mobs.sort_by(|a, b| a.id.cmp(&b.id));
        for mob in mobs {
            snapshot.entities.push(mob.entity());
        }
        let mut corpses: Vec<&LiveCorpse> = self
            .corpses
            .values()
            .filter(|c| c.location_id == loc_id)
            .collect();
        corpses.sort_by(|a, b| a.id.cmp(&b.id));
        for corpse in corpses {
            snapshot.entities.push(corpse.entity());
        }
        // M20: предметы, лежащие в локации (выбросы) — как объекты с
        // маркером ground (подбор через `cmd/item.use` по id).
        let mut ground: Vec<&GroundItem> = self
            .ground_items
            .values()
            .filter(|g| g.location_id == loc_id)
            .collect();
        ground.sort_by(|a, b| a.id.cmp(&b.id));
        for item in ground {
            let mut extra = serde_json::Map::new();
            extra.insert("ground".into(), serde_json::json!(true));
            extra.insert("def_id".into(), serde_json::json!(item.def_id));
            extra.insert("qty".into(), serde_json::json!(item.qty));
            snapshot.entities.push(Entity {
                id: item.id.clone(),
                kind: EntityKind::Object,
                name: item.name.clone(),
                hp: None,
                tags: None,
                was_player: None,
                aggro: None,
                boss: None,
                next_attack_at: None,
                attack_interval: None,
                attacking: None,
                extra,
            });
        }
        // M20/F20b: ресурсы (скрытые — до респавна не показываются).
        let mut nodes: Vec<&ResourceNode> = self
            .resources
            .values()
            .filter(|r| r.location_id == loc_id && !r.hidden)
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        for node in nodes {
            snapshot.entities.push(node.entity());
        }
        snapshot
    }

    /// Статический лут сущности сущности (для `entity.info`/`take_loot`:
    /// сначала динамические трупы, затем статика из row).
    pub fn dynamic_corpse(&self, entity_id: &str) -> Option<&LiveCorpse> {
        self.corpses.get(entity_id)
    }

    pub fn mob(&self, mob_id: &str) -> Option<&LiveMob> {
        self.mobs.get(mob_id)
    }

    /// Нанести урон мобу; `true` — моб убит (труп создаётся вызывающим).
    pub fn damage_mob(&mut self, mob_id: &str, amount: i64) -> Option<i64> {
        let mob = self.mobs.get_mut(mob_id)?;
        mob.hp = (mob.hp - amount).max(0);
        Some(mob.hp)
    }

    /// Убить моба → динамический труп с его лутом; id трупа.
    pub fn kill_mob(&mut self, mob_id: &str) -> Option<String> {
        let mob = self.mobs.remove(mob_id)?;
        self.corpse_seq += 1;
        let corpse_id = format!("cor_d{}", self.corpse_seq);
        // M20: таблица лута — в труп попадают только выпавшие по шансу
        // (строки старого формата — всегда); M24: админ-лут монстра
        // добавляется сверху (одноразово, до респавна).
        let mut loot: Vec<String> = mob
            .drops
            .iter()
            .filter(|d| roll_chance(d.chance))
            .map(|d| d.def_id.clone())
            .collect();
        loot.extend(mob.bonus_loot.iter().cloned());
        self.corpses.insert(
            corpse_id.clone(),
            LiveCorpse {
                id: corpse_id.clone(),
                location_id: mob.location_id.clone(),
                name: format!("Труп: {}", mob.name),
                loot: loot.into_iter().map(Some).collect(),
                born_at: crate::time::now_epoch(),
                harvest: mob.harvest.clone(),
                skin_done: false,
                gut_done: false,
                inner_loot: Vec::new(),
            },
        );
        // M20: респавн-диапазон моба → очередь возрождений.
        if let Some((min_s, max_s)) = mob.respawn {
            let at = crate::time::now_epoch() + rand_range(min_s, max_s);
            self.respawn_queue.push(RespawnEntry {
                entity_id: mob.id.clone(),
                location_id: mob.location_id.clone(),
                kind: RespawnKind::Mob {
                    corpse_id: Some(corpse_id.clone()),
                },
                at,
            });
        }
        Some(corpse_id)
    }

    // ─── M20: предметы в локациях (выбросы) и TTL трупов ────────────

    /// Бросить предмет в локацию (item.drop); id наземного предмета.
    pub fn drop_ground(
        &mut self,
        location_id: &str,
        name: &str,
        def_id: &str,
        qty: i64,
        dropped_by: &str,
        expires_at: i64,
    ) -> String {
        self.ground_seq += 1;
        let id = format!("g{}", self.ground_seq);
        self.ground_items.insert(
            id.clone(),
            GroundItem {
                id: id.clone(),
                location_id: location_id.to_string(),
                name: name.to_string(),
                def_id: def_id.to_string(),
                qty,
                dropped_by: dropped_by.to_string(),
                expires_at,
            },
        );
        id
    }

    pub fn ground_item(&self, uid: &str) -> Option<&GroundItem> {
        self.ground_items.get(uid)
    }

    /// Подобрать наземный предмет (удалить из локации).
    pub fn take_ground(&mut self, uid: &str) -> Option<GroundItem> {
        self.ground_items.remove(uid)
    }

    /// M20: просроченные наземные предметы — удалить и вернуть (для
    /// рассылки «{name} исчез»); `ttl <= 0` — не удалять (выключено).
    pub fn due_ground(&mut self, now: i64, ttl_s: i64) -> Vec<GroundItem> {
        if ttl_s <= 0 {
            return Vec::new();
        }
        let due: Vec<String> = self
            .ground_items
            .values()
            .filter(|g| now >= g.expires_at)
            .map(|g| g.id.clone())
            .collect();
        due.into_iter()
            .filter_map(|id| self.ground_items.remove(&id))
            .collect()
    }

    /// M20: трупы старше `corpse_ttl_s` — удалить и вернуть (для
    /// рассылки despawn); `ttl <= 0` — не удалять.
    pub fn due_corpses(&mut self, now: i64, ttl_s: i64) -> Vec<LiveCorpse> {
        if ttl_s <= 0 {
            return Vec::new();
        }
        let due: Vec<String> = self
            .corpses
            .values()
            .filter(|c| now >= c.born_at + ttl_s)
            .map(|c| c.id.clone())
            .collect();
        due.into_iter()
            .filter_map(|id| self.corpses.remove(&id))
            .collect()
    }

    // ─── Снятие шкур: сервер-джобы разделки (5 с) ─────────────────────────

    pub fn harvest_job(&self, char_id: &str) -> Option<&HarvestJob> {
        self.harvest_jobs.get(char_id)
    }

    /// Запустить разделку (вызывающий проверил все условия).
    pub fn start_harvest(&mut self, job: HarvestJob) {
        self.harvest_jobs.insert(job.char_id.clone(), job);
    }

    /// M24: идёт ли у персонажа активная разделка (блок действий,
    /// `E_BUSY`; item-команды — исключение, прерывают по M23).
    pub fn is_harvesting(&self, char_id: &str) -> bool {
        self.harvest_jobs.contains_key(char_id)
    }

    /// M24: прервать активную разделку (взаимодействие с предметом)
    /// → джоб изымается; для skin вызывающий помечает труп
    /// «шкура испорчена» ([`Self::ruin_skin`]).
    pub fn cancel_harvest(&mut self, char_id: &str) -> Option<HarvestJob> {
        self.harvest_jobs.remove(char_id)
    }

    /// M22: шкура испорчена — повторное снятие с трупа невозможно
    /// (труп мог уже истечь — тогда просто no-op).
    pub fn ruin_skin(&mut self, corpse_id: &str) {
        if let Some(corpse) = self.corpses.get_mut(corpse_id) {
            corpse.skin_done = true;
        }
    }

    /// Джобы, срок которых наступил (извлекаются из очереди).
    pub fn due_harvests(&mut self, now: i64) -> Vec<HarvestJob> {
        let due: Vec<String> = self
            .harvest_jobs
            .iter()
            .filter(|(_, j)| j.at <= now)
            .map(|(c, _)| c.clone())
            .collect();
        due.into_iter()
            .filter_map(|c| self.harvest_jobs.remove(&c))
            .collect()
    }

    /// Завершить разделку: отметить «сделано» и добавить лут —
    /// `skin`: шкура со шансом `hide_chance(level)` иначе обрезки,
    /// **качество = ранг** (`hide_q{rank}`/`hide_scraps_q{rank}`),
    /// крит по рангу → **+2 шкуры** сверх; `gut`: мясо (25 %) либо
    /// потроха → в **отдельный** `inner_loot` («Внутри»).
    /// → `(обновлённый труп, [шкуры], [внутри])`; `None` — труп
    /// исчез или действие уже сделано.
    pub fn finish_harvest(
        &mut self,
        corpse_id: &str,
        action: &str,
        level: u32,
        rank: u32,
    ) -> Option<(LiveCorpse, Vec<String>, Vec<String>)> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let rank = rank.clamp(1, 5);
        match action {
            "skin" => {
                if corpse.skin_done {
                    return None;
                }
                corpse.skin_done = true;
                let def = if roll_chance(crate::systems::hide_chance(level)) {
                    format!("hide_q{rank}")
                } else {
                    format!("hide_scraps_q{rank}")
                };
                let mut skin = vec![def.clone()];
                corpse.loot.push(Some(def));
                // Крит (шанс по рангу): +2 шкуры текущего качества.
                if roll_chance(crate::systems::harvest_crit(rank)) {
                    for _ in 0..2 {
                        let d = format!("hide_q{rank}");
                        corpse.loot.push(Some(d.clone()));
                        skin.push(d);
                    }
                }
                Some((corpse.clone(), skin, Vec::new()))
            }
            "gut" => {
                if corpse.gut_done {
                    return None;
                }
                corpse.gut_done = true;
                let def = if roll_chance(0.25) { "meat" } else { "offal" };
                corpse.inner_loot.push(Some(def.to_string()));
                Some((corpse.clone(), Vec::new(), vec![def.to_string()]))
            }
            _ => None,
        }
    }

    /// Взять слот лута трупа (`loot:` — общий список, `inner:` —
    /// «Внутри»); `None` — уже взят/нет слота.
    pub fn take_dynamic_slot(
        &mut self,
        corpse_id: &str,
        index: usize,
        inner: bool,
    ) -> Option<String> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let slot = if inner {
            corpse.inner_loot.get_mut(index)?
        } else {
            corpse.loot.get_mut(index)?
        };
        slot.take()
    }

    // ─── M20: очередь возрождений ──────────────────────────────────────────

    /// M20: статический лут сущности взят → возродить через её
    /// `respawn {min_s,max_s}` (нет поля — однократно); повторные
    /// взятия не плодят дубли записи в очереди.
    pub fn schedule_loot_respawn(&mut self, entity_id: &str) {
        let Some((min_s, max_s)) = self.entity_respawn.get(entity_id).copied() else {
            return;
        };
        let pending = self
            .respawn_queue
            .iter()
            .any(|e| e.entity_id == entity_id && matches!(e.kind, RespawnKind::Loot));
        if pending {
            return;
        }
        let at = crate::time::now_epoch() + rand_range(min_s, max_s);
        let location_id = String::new(); // лут-сбросу локация не нужна
        self.respawn_queue.push(RespawnEntry {
            entity_id: entity_id.to_string(),
            location_id,
            kind: RespawnKind::Loot,
            at,
        });
    }

    /// M20: возрождения, срок которых наступил (по возрастанию `at`);
    /// записи извлекаются — применение (возврат моба/сброс лута) за
    /// вызывающим (тик-шаг `respawn_step`).
    pub fn due_respawns(&mut self, now: i64) -> Vec<RespawnEntry> {
        let (mut due, keep): (Vec<_>, Vec<_>) =
            self.respawn_queue.drain(..).partition(|e| now >= e.at);
        self.respawn_queue = keep;
        due.sort_by_key(|e| e.at);
        due
    }

    /// M20: вернуть моба из шаблона сида (стартовый HP) — проводной
    /// вид для `evt/entity/spawn`; `None` — нет шаблона (не должен
    /// случиться: шаблон живёт с сида).
    pub fn respawn_mob(&mut self, entity_id: &str) -> Option<Entity> {
        let mut mob = self.mob_seed.get(entity_id)?.clone();
        mob.hp = mob.hp_start;
        let entity = mob.entity();
        self.mobs.insert(entity_id.to_string(), mob);
        Some(entity)
    }

    /// M20: удалить старый труп погибшего моба (вместе с лутом — по
    /// возврату респавна); `None` — уже удалён (TTL).
    pub fn remove_corpse(&mut self, corpse_id: &str) -> Option<LiveCorpse> {
        self.corpses.remove(corpse_id)
    }

    // ─── M20/F20b: объекты-ресурсы ────────────────────────────────────────

    pub fn resource(&self, entity_id: &str) -> Option<&ResourceNode> {
        self.resources.get(entity_id)
    }

    /// M20/F20b: добыть 1 единицу (валидации инструмента/ранга — на
    /// сервере до вызова): `Some(узел после)`; `None` — нет в этой
    /// локации / скрыт / исчерпан.
    pub fn try_gather(&mut self, entity_id: &str, location_id: &str) -> Option<ResourceNode> {
        let node = self.resources.get_mut(entity_id)?;
        if node.location_id != location_id || node.hidden || node.current <= 0 {
            return None;
        }
        node.current -= 1;
        let exhausted = node.current == 0;
        let (min_s, max_s, despawn) = (node.respawn.0, node.respawn.1, node.despawn_when_empty);
        if exhausted {
            // Респавн до max — по тому же диапазону min..max.
            let at = crate::time::now_epoch() + rand_range(min_s, max_s);
            self.respawn_queue.push(RespawnEntry {
                entity_id: entity_id.to_string(),
                location_id: location_id.to_string(),
                kind: RespawnKind::Resource,
                at,
            });
            if despawn {
                node.hidden = true;
            }
        }
        Some(node.clone())
    }

    /// M20/F20b: восстановить ресурс до max (шаг `respawn_step`);
    /// `(узел, был_скрыт)` — был_скрыт → `evt/entity/spawn`, иначе
    /// патч шкалы через `evt/entity/move`.
    pub fn respawn_resource(&mut self, entity_id: &str) -> Option<(ResourceNode, bool)> {
        let node = self.resources.get_mut(entity_id)?;
        let was_hidden = node.hidden;
        node.current = node.max;
        node.hidden = false;
        Some((node.clone(), was_hidden))
    }

    /// Взять лот-слот из динамического трупа; `None` — уже взят/нет слота.
    pub fn take_dynamic_loot(&mut self, corpse_id: &str, index: usize) -> Option<String> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let slot = corpse.loot.get_mut(index)?;
        slot.take()
    }

    /// Мобы локации, готовые атаковать (агрессивные — при живых персонажах).
    pub fn aggro_mobs_in(&self, location_id: &str) -> Vec<String> {
        self.mobs
            .values()
            .filter(|m| m.location_id == location_id && m.aggro)
            .map(|m| m.id.clone())
            .collect()
    }

    pub fn mob_location(&self, mob_id: &str) -> Option<String> {
        self.mobs.get(mob_id).map(|m| m.location_id.clone())
    }

    /// Пара (цель, локация) боя персонажа.
    pub fn combat_of(&self, char_id: &str) -> Option<(String, String)> {
        self.combat
            .get(char_id)
            .map(|c| (c.target_id.clone(), c.location_id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M24: «режим бога» — по-персонажный toggle неуязвимости
    /// (tick::apply_mob_hit смотрит `is_god` до применения урона).
    #[test]
    fn god_mode_toggles_per_char() {
        let mut world = World::default();
        assert!(!world.is_god("c1"), "по умолчанию — смертный");
        world.set_god("c1", true);
        assert!(world.is_god("c1"), "включён");
        assert!(!world.is_god("c2"), "не глобально");
        world.set_god("c1", false);
        assert!(!world.is_god("c1"), "выключен");
    }

    /// M20/F20a: `rand_range` — всегда в диапазоне [min, max]
    /// (респавн-таймеры задаются двумя числами).
    #[test]
    fn rand_range_stays_within_bounds() {
        for _ in 0..200 {
            let v = rand_range(3, 7);
            assert!((3..=7).contains(&v), "вне диапазона: {v}");
        }
        assert_eq!(rand_range(5, 5), 5, "равные границы");
        assert_eq!(rand_range(7, 3), 7, "max < min → min");
        assert_eq!(rand_range(-5, -1), 0, "отрицательные → 0");
    }

    /// M20/F20a: очередь возрождений — моб возвращается из шаблона
    /// сида со стартовым HP (старый труп удаляется), лут-сброс
    /// планируется один раз (дедуп повторных взятий).
    #[test]
    fn respawn_schedules_and_restores_mob() {
        let mut world = World::default();
        let row = (
            "l_test".to_string(),
            serde_json::json!([
                {"id": "m_r", "kind": "monster", "name": "Крыса",
                 "hp": {"cur": 6, "max": 6}, "difficulty": 1,
                 "respawn": {"min_s": 5, "max_s": 10}},
                {"id": "e_chest", "kind": "object", "name": "Сундук",
                 "loot": ["gold_coin"],
                 "respawn": {"min_s": 2, "max_s": 4}}
            ]),
        );
        world.seed_mobs(&[row], None);
        let base = crate::time::now_epoch();

        // Лут: взяли → одна запись в очереди (повторные — дедуп).
        world.schedule_loot_respawn("e_chest");
        world.schedule_loot_respawn("e_chest");
        // Моб убит → запись с трупом; до срока очередь пуста.
        let corpse = world.kill_mob("m_r").expect("труп создаётся");
        assert!(world.mob("m_r").is_none(), "моб мёртв");
        assert!(
            world.due_respawns(base + 1).is_empty(),
            "до минимального срока (2 с) — пусто"
        );

        // Срок прошёл: обе записи, время в диапазонах контента.
        let due = world.due_respawns(base + 60);
        assert_eq!(due.len(), 2, "лут и моб по сроку: {due:?}");
        let loot = due
            .iter()
            .find(|e| matches!(e.kind, RespawnKind::Loot))
            .expect("лут-запись");
        assert_eq!(loot.entity_id, "e_chest");
        let mob_e = due
            .iter()
            .find(|e| matches!(e.kind, RespawnKind::Mob { .. }))
            .expect("моб-запись");
        assert_eq!(mob_e.entity_id, "m_r");
        assert_eq!(mob_e.location_id, "l_test");
        assert!((base..=base + 10).contains(&mob_e.at), "at в [5,10]");
        assert!(
            matches!(
            &mob_e.kind,
            RespawnKind::Mob {
                corpse_id: Some(c)
            } if c == &corpse
            ),
            "запись несёт труп: {:?}",
            mob_e.kind
        );

        // Моб: снова в локации, стартовый HP; труп удалён; очередь пуста.
        let entity = world.respawn_mob("m_r").expect("возврат из шаблона");
        assert_eq!(entity.id, "m_r");
        assert_eq!(entity.hp.map(|h| h.cur), Some(6));
        assert!(world.mob("m_r").is_some());
        assert!(world.remove_corpse(&corpse).is_some(), "старый труп ушёл");
        assert!(world.remove_corpse(&corpse).is_none(), "уже удалён");
        assert!(
            world.due_respawns(base + 600).is_empty(),
            "очередь пуста (дедуп лута)"
        );
    }

    /// Сущности локации из снапшота (`build_location` мутирует вход).
    fn loc_entities(world: &World, loc_id: &str) -> Vec<ld2_protocol::types::Entity> {
        world
            .build_location(ld2_protocol::types::LocationSnapshot {
                id: loc_id.to_string(),
                name: loc_id.to_string(),
                flags: Vec::new(),
                text: String::new(),
                exits: Vec::new(),
                entities: Vec::new(),
                extra: Default::default(),
            })
            .entities
    }

    /// M20/F20b: ресурс убывает по 1 за добычу; при 0 — респавн
    /// min..max; трава (despawn_when_empty) скрывается, жила остаётся
    /// видимой и исчерпанной до возобновления.
    #[test]
    fn resource_gather_exhausts_and_respawns() {
        let mut world = World::default();
        let row = (
            "l_t".to_string(),
            serde_json::json!([
                {"id": "r_vein", "kind": "resource", "name": "Жила",
                 "resource": "ore_copper", "max": 2,
                 "respawn": {"min_s": 5, "max_s": 10},
                 "despawn_when_empty": false,
                 "profession": "mining", "min_rank": 1, "tool": "pickaxe"},
                {"id": "r_herb", "kind": "resource", "name": "Трава",
                 "resource": "herb", "max": 1,
                 "respawn": {"min_s": 30, "max_s": 60},
                 "despawn_when_empty": true,
                 "profession": "herbalist", "min_rank": 1, "tool": "sickle"}
            ]),
        );
        world.seed_mobs(&[row], None);

        // Стартовый запас = max, параметры из контента, в снапшоте есть.
        let vein = world.resource("r_vein").expect("жида");
        assert_eq!((vein.current, vein.max), (2, 2));
        assert_eq!(vein.tool.as_deref(), Some("pickaxe"));
        assert!(loc_entities(&world, "l_t")
            .iter()
            .any(|e| e.id == "r_vein" && e.kind == EntityKind::Resource));

        // Добыча: −1 за действие; чужая локация — отказ.
        assert!(
            world.try_gather("r_vein", "l_x").is_none(),
            "не в своей локации"
        );
        assert_eq!(world.try_gather("r_vein", "l_t").unwrap().current, 1);
        // Исчерпание: жила остаётся видимой, в очереди респавн min..max.
        let after = world.try_gather("r_vein", "l_t").unwrap();
        assert_eq!(after.current, 0);
        assert!(!after.hidden, "жида остаётся на месте");
        let base = crate::time::now_epoch();
        assert!(world.due_respawns(base + 4).is_empty(), "до min (5 с)");
        let due = world.due_respawns(base + 60);
        assert!(
            due.iter().any(|e| e.entity_id == "r_vein"
                && matches!(e.kind, RespawnKind::Resource)
                && (base..=base + 10).contains(&e.at)),
            "респавн в [5,10]: {due:?}"
        );
        let (node, was_hidden) = world.respawn_resource("r_vein").unwrap();
        assert!(!was_hidden, "жида не скрывалась");
        assert_eq!(node.current, 2, "восстановление до max");

        // Трава: исчерпание скрывает до респавна (despawn_when_empty).
        let after = world.try_gather("r_herb", "l_t").unwrap();
        assert_eq!((after.current, after.hidden), (0, true));
        assert!(
            !loc_entities(&world, "l_t").iter().any(|e| e.id == "r_herb"),
            "исчерпанная трава скрыта"
        );
        assert!(
            world
                .due_respawns(base + 60)
                .iter()
                .any(|e| e.entity_id == "r_herb"),
            "респавн травы в очереди"
        );
        let (node, was_hidden) = world.respawn_resource("r_herb").unwrap();
        assert!(was_hidden, "была скрыта");
        assert_eq!((node.current, node.hidden), (1, false), "вернулась");
    }

    /// M20: дефолт `[world]` применяется только к мобам без своего
    /// `respawn`; сущности без respawn и дефолта не возрождаются.
    #[test]
    fn respawn_default_fills_only_unconfigured_mobs() {
        let mut world = World::default();
        let row = (
            "l_t".to_string(),
            serde_json::json!([
                {"id": "m_a", "kind": "monster", "name": "A",
                 "hp": {"cur": 3, "max": 3}},
                {"id": "m_b", "kind": "monster", "name": "B",
                 "hp": {"cur": 3, "max": 3},
                 "respawn": {"min_s": 40, "max_s": 50}},
                {"id": "e_c", "kind": "object", "name": "C"}
            ]),
        );
        world.seed_mobs(&[row], Some((60, 120)));
        assert_eq!(world.entity_respawn.get("m_a"), Some(&(60, 120)));
        assert_eq!(world.entity_respawn.get("m_b"), Some(&(40, 50)));
        assert!(
            !world.entity_respawn.contains_key("e_c"),
            "дефолт не для не-мобов"
        );
    }

    /// M18/F58d: дебаффы рун — стан блокирует атаки до срока, bleed
    /// тикает по расписанию, корни хранятся (движение — позже).
    #[test]
    fn rune_debuffs_stun_bleed_root_lifecycle() {
        let mut world = World::default();
        let row = (
            "l_forest".to_string(),
            serde_json::json!([{
                "id": "m_w", "kind": "monster", "name": "Волк",
                "hp": { "cur": 18, "max": 18 },
                "difficulty": 3, "aggro": true, "damage": 6
            }]),
        );
        world.seed_mobs(&[row], None);
        let now = 1_000_000_i64;

        // Стан: активен ровно duration, потом снимается.
        assert!(!world.mob_stunned("m_w", now), "до эффекта не стан");
        world.apply_rune_effect("m_w", "stun", 0, 3, "c1", now);
        assert!(world.mob_stunned("m_w", now + 2_999), "стан активен");
        assert!(!world.mob_stunned("m_w", now + 3_001), "стан истёк");

        // Кровотечение: первый тик через секунду, dps из контента,
        // источник сохранён для XP.
        world.apply_rune_effect("m_w", "bleed", 3, 6, "c1", now);
        assert_eq!(world.bleeding_mobs(now), Vec::new(), "тиков ещё не было");
        let due = world.bleeding_mobs(now + 1_000);
        assert_eq!(due.len(), 1, "пора тикнуть");
        assert_eq!(due[0].1, 3, "dps из эффекта");
        assert_eq!(due[0].3.as_deref(), Some("c1"), "источник для XP");
        world.advance_bleed_tick("m_w", now + 1_000);
        assert_eq!(
            world.bleeding_mobs(now + 1_500),
            Vec::new(),
            "следующий тик только через секунду"
        );

        // Срок истёк — снимаем (bleed_dps обнуляется).
        world.apply_rune_effect("m_w", "bleed", 3, 2, "c1", now);
        world.advance_bleed_tick("m_w", now + 5_000);
        let mobs = world.bleeding_mobs(now + 6_000);
        assert!(mobs.is_empty(), "истёкший DoT не тикает: {mobs:?}");

        // Регрессия: частые пере-наложения (шанс 1.0) продлевают срок,
        // но НЕ сбрасывают тик — иначе DoT никогда не дотикает.
        world.apply_rune_effect("m_w", "bleed", 3, 6, "c1", now);
        let next_before = world.bleeding_mobs(now + 1_000);
        assert_eq!(next_before.len(), 1, "тикер назначен");
        world.apply_rune_effect("m_w", "bleed", 3, 6, "c1", now + 500);
        world.apply_rune_effect("m_w", "bleed", 3, 6, "c1", now + 900);
        let due = world.bleeding_mobs(now + 1_000);
        assert_eq!(due.len(), 1, "тик не сброшен пере-наложением");
        world.advance_bleed_tick("m_w", now + 1_000);

        // Корни: поле хранится (перемещение мобов появится позже) и не
        // путается со станом — стан к этому моменту уже истёк.
        world.apply_rune_effect("m_w", "root", 0, 5, "c1", now);
        assert!(
            !world.mob_stunned("m_w", now + 10_000),
            "root не блокирует атаки (≠ стан)"
        );
    }

    /// M17/F30g: период атаки — из cooldown_s оружия; снятие оружия
    /// убирает значение (в plan берётся конфиг).
    #[test]
    fn char_attack_ms_set_get_and_clear() {
        let mut world = World::default();
        assert_eq!(world.char_attack_ms("c1"), None, "до входа — конфиг");
        world.set_char_attack_ms("c1", Some(1_500));
        assert_eq!(world.char_attack_ms("c1"), Some(1_500));
        world.set_char_attack_ms("c1", None);
        assert_eq!(world.char_attack_ms("c1"), None, "оружие снято");
    }
}

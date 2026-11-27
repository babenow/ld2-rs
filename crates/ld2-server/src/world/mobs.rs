//! Мобы, боссы и таблицы дропа/респавна мира (D26/M20/M25).

use super::*;
use crate::consts::{EFFECT_BLEED, EFFECT_BLIND, EFFECT_SILENCE, EFFECT_STUN, MS_PER_SECOND};

const RANDOM_BUF_LEN: usize = 2;
const DEFAULT_SUMMON_COUNT: i64 = 2;
const MIN_INTERVAL_MULT: f64 = 0.05;
const DEFAULT_ACCURACY: i64 = 75;

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
    let mut buf = [0u8; RANDOM_BUF_LEN];
    if getrandom::fill(&mut buf).is_err() {
        return true; // без RNG лут не теряем (как раньше — всегда)
    }
    (u16::from_be_bytes(buf) as f64) / (u16::MAX as f64) < chance
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

/// M37: призыв мобов приёмом (Белый волк — «Громкий вой»).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BossSummon {
    /// id шаблона моба в контенте (`mob_seed`), например `m_3`.
    pub mob: String,
    #[serde(default = "boss_summon_count")]
    pub count: i64,
}

fn boss_summon_count() -> i64 {
    DEFAULT_SUMMON_COUNT
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
    /// M25: эффект на игроков при применении (стан/немота/ослепление,
    /// M37 — кровотечение).
    #[serde(default)]
    pub effect: Option<BossEffect>,
    /// M37: призыв мобов при применении («Громкий вой»).
    #[serde(default)]
    pub summon: Option<BossSummon>,
}

/// M25: эффект приёма босса на игроков локации.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BossEffect {
    /// `stun` | `silence` | `blind` | `bleed` (M37).
    pub kind: String,
    pub duration_s: i64,
    /// M37: для `bleed` — урон в секунду (по умолчанию 1).
    #[serde(default)]
    pub value: Option<i64>,
}

/// M25: название эффекта контроля (сервер — истина, RU).
pub fn effect_title(kind: &str) -> String {
    match kind {
        EFFECT_STUN => "Оглушение".to_string(),
        EFFECT_SILENCE => "Немота".to_string(),
        EFFECT_BLIND => "Ослепление".to_string(),
        EFFECT_BLEED => "Кровотечение".to_string(),
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
            self.buff_until = now_ms + buff.duration_s.max(1) * MS_PER_SECOND;
            self.buff_damage_mult = buff.damage_mult.max(0.0);
            self.buff_interval_mult = buff.interval_mult.max(MIN_INTERVAL_MULT);
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
    /// M32: боевые статы моба (content `combat` или плоские поля;
    /// дефолты — от сложности 1..10).
    /// Броня — плоское снижение физического урона.
    pub armor: i64,
    /// Уклонение, % (0..60) — шанс увернуться от удара игрока.
    pub dodge: i64,
    /// Парирование, % (0..60) — шанс вдвое снизить удар.
    pub parry: i64,
    /// Магическое сопротивление, % (0..80).
    pub resist: i64,
    /// Точность (1..99) — шанс попасть по игроку.
    pub accuracy: i64,
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
    /// R-контент: «горение» — DoT рун с настраиваемым периодом
    /// (`stats.effect: {kind: "burn", value, period_s, duration_s}`).
    pub burn_dps: i64,
    pub burn_until: i64,
    pub burn_next_at: i64,
    pub burn_period: i64,
    /// Кто наложил горение — XP за добивание от DoT.
    pub burn_src: Option<String>,
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
        // M32: боевые статы — в `extra` (аддитивно §6): клиентские
        // подсказки/карточка моба показывают защиту.
        let mut extra = serde_json::Map::new();
        extra.insert("difficulty".into(), serde_json::json!(self.difficulty));
        extra.insert("armor".into(), serde_json::json!(self.armor));
        extra.insert("dodge".into(), serde_json::json!(self.dodge));
        extra.insert("parry".into(), serde_json::json!(self.parry));
        extra.insert("resist".into(), serde_json::json!(self.resist));
        extra.insert("accuracy".into(), serde_json::json!(self.accuracy));
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
            extra,
        }
    }
}

impl World {
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
    /// R-контент: горящие мобы, у которых пора тикнуть → `(id, dps, src)`.
    pub fn burning_mobs(&self, now_ms: i64) -> Vec<(String, i64, Option<String>)> {
        self.mobs
            .values()
            .filter(|m| m.burn_dps > 0 && m.burn_until >= now_ms && m.burn_next_at <= now_ms)
            .map(|m| (m.id.clone(), m.burn_dps, m.burn_src.clone()))
            .collect()
    }
    /// R-контент: продвинуть тик горения (следующий — через период).
    pub fn advance_burn_tick(&mut self, mob_id: &str, now_ms: i64) {
        if let Some(mob) = self.mobs.get_mut(mob_id) {
            mob.burn_next_at = now_ms + mob.burn_period.max(MS_PER_SECOND);
            if mob.burn_until <= now_ms {
                mob.burn_dps = 0;
            }
        }
    }
    /// M18/F58d: продвинуть тик кровоточащего моба (следующий через 1 с).
    pub fn advance_bleed_tick(&mut self, mob_id: &str, now_ms: i64) {
        if let Some(mob) = self.mobs.get_mut(mob_id) {
            mob.bleed_next_at = now_ms + MS_PER_SECOND;
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
            .rposition(|p| fight_ms >= p.after_s.saturating_mul(MS_PER_SECOND))
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
            Some(p) => (
                p.damage_mult.max(0.0),
                p.interval_mult.max(MIN_INTERVAL_MULT),
            ),
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
                boss.next_skill_at = now_ms + cast.skill.cooldown_s.max(1) * MS_PER_SECOND;
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
            boss.next_skill_at = now_ms + skill.cooldown_s.max(1) * MS_PER_SECOND;
            boss.apply_self_buff(&skill, now_ms);
            return (BossSkillTick::Applied(skill), false);
        }
        // Каст: во время подготовки босс не атакует.
        let ends_at = now_ms + skill.cast_s.max(1) * MS_PER_SECOND;
        boss.next_skill_at = ends_at + skill.cooldown_s.max(1) * MS_PER_SECOND;
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
    /// M32: боевая защита моба → (броня, уклонение %, парирование %,
    /// магическое сопротивление %, точность).
    pub fn mob_defense(&self, mob_id: &str) -> (i64, i64, i64, i64, i64) {
        self.mobs
            .get(mob_id)
            .map(|m| (m.armor, m.dodge, m.parry, m.resist, m.accuracy))
            .unwrap_or((0, 0, 0, 0, DEFAULT_ACCURACY))
    }
    /// M32: магическое сопротивление моба, %.
    pub fn mob_resist(&self, mob_id: &str) -> i64 {
        self.mobs.get(mob_id).map(|m| m.resist).unwrap_or(0)
    }
    pub fn mob_max_hp(&self, mob_id: &str) -> i64 {
        self.mobs.get(mob_id).map(|m| m.hp_max).unwrap_or(0)
    }
    /// Количество живых мобов (лог старта).
    pub fn mob_count(&self) -> usize {
        self.mobs.len()
    }
    /// Прервать бой персонажа (смена локации, стоп, смерть).
    pub fn end_combat(&mut self, char_id: &str) -> bool {
        self.combat.remove(char_id).is_some()
    }
    pub fn in_combat(&self, char_id: &str) -> bool {
        self.combat.contains_key(char_id)
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
    /// M37: мобы, выбранные целью активного боя (бьют в ответ, даже
    /// если не агрессивны — например, элитный Белый волк).
    pub fn combat_mobs_in(&self, location_id: &str) -> Vec<String> {
        self.combat
            .values()
            .filter(|c| c.location_id == location_id)
            .map(|c| c.target_id.clone())
            .collect()
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

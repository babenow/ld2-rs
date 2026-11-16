//! Мир в памяти (D31, M12.5): боевое состояние, мобы/трупы локаций и
//! реестр соединений для серверной рассылки (в т.ч. событий тика боя).
//!
//! Мобы и динамические трупы **эфемерны**: при рестарте воссоздаются из
//! сида (D26) — их hp не переживает перезапуск (отклонение M12.5).
//! Персонажи, инвентарь и лут статических трупов — в БД (D27).

use std::collections::HashMap;
use tokio::sync::mpsc::UnboundedSender as SyncSender;

use ld2_protocol::types::{Entity, EntityKind, Hp};

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
    /// Определения лута (content `drops`) — попадают в труп.
    pub drops: Vec<String>,
    pub tags: Vec<String>,
    pub aggro: bool,
    /// Урон моба (content `damage`, по умолчанию 4).
    pub damage: i64,
    /// Следующая атака моба (epoch, мс).
    pub next_attack_at: i64,
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
}

impl LiveCorpse {
    /// Проводной вид §6 (счётчик лута — в `evt/corpse/created.loot_count`).
    pub fn entity(&self) -> Entity {
        Entity {
            id: self.id.clone(),
            kind: EntityKind::Corpse,
            name: self.name.clone(),
            hp: None,
            tags: None,
            was_player: None,
            aggro: None,
            extra: Default::default(),
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
    conns: HashMap<String, SyncSender<String>>,
    /// char_id → имя (для боевых строк §8).
    char_names: HashMap<String, String>,
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
}

impl World {
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
    pub fn seed_mobs(&mut self, rows: &[(String, serde_json::Value)]) {
        for (location_id, entities) in rows {
            let entities: Vec<Entity> =
                serde_json::from_value(entities.clone()).unwrap_or_default();
            for e in entities {
                if e.kind != EntityKind::Monster {
                    continue;
                }
                let extra = &e.extra;
                let difficulty = extra
                    .get("difficulty")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1)
                    .clamp(1, 10);
                let drops = extra
                    .get("drops")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let damage = extra.get("damage").and_then(|v| v.as_i64()).unwrap_or(4);
                self.mobs.insert(
                    e.id.clone(),
                    LiveMob {
                        id: e.id.clone(),
                        location_id: location_id.clone(),
                        name: e.name.clone(),
                        hp: e.hp.map(|hp| hp.cur).unwrap_or(1),
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
                    },
                );
            }
        }
    }

    /// Регистрация соединения персонажа (рассылка событий тика §2.4).
    pub fn bind_char(&mut self, char_id: &str, name: &str, tx: SyncSender<String>) {
        self.conns.insert(char_id.to_string(), tx);
        self.char_names
            .insert(char_id.to_string(), name.to_string());
    }

    pub fn unbind_char(&mut self, char_id: &str) {
        self.conns.remove(char_id);
        self.char_names.remove(char_id);
        self.char_loc.remove(char_id);
        self.combat.remove(char_id);
        self.ability_cd.retain(|(c, _), _| c != char_id);
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
            let _ = tx.send(text);
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
        snapshot.entities.retain(|e| e.kind != Monster);
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
        self.corpses.insert(
            corpse_id.clone(),
            LiveCorpse {
                id: corpse_id.clone(),
                location_id: mob.location_id.clone(),
                name: format!("Труп: {}", mob.name),
                loot: mob.drops.into_iter().map(Some).collect(),
            },
        );
        Some(corpse_id)
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
        world.seed_mobs(&[row]);
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

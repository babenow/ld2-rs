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
    corpse_seq: u64,
}

impl World {
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

//! Редьюсеры боя (RF4e): удары/кулдауны, действия мобов, магия,
//! результат сбора и аффликты. Вынесено из `apply_event` (§7), тела
//! веток перенесены как есть.

use ld2_protocol::names;

use super::super::*;

impl AppStore {
    /// Диспетчер боевых событий: combat/*, mob/*, magic, harvest, afflict.
    pub(crate) fn reduce_combat(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Vec<Effect> {
        match msg_type {
            names::EVT_COMBAT_HIT | names::EVT_COMBAT_COOLDOWN => {
                self.reduce_combat_events(msg_type, payload)
            }
            m if m.starts_with(names::EVT_MOB_PREFIX) => self.reduce_mob(m, payload),
            names::EVT_MAGIC_CAST | names::EVT_HARVEST_RESULT | names::EVT_CHAR_AFFLICT => {
                self.reduce_effects(msg_type, payload)
            }
            _ => vec![],
        }
    }

    fn reduce_combat_events(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_COMBAT_COOLDOWN => {
                // ФР-05: блокировка действия до until_epoch.
                let Ok(cd) = serde_json::from_value::<CombatCooldown>(payload) else {
                    return vec![];
                };
                if let Some(skill) = cd.skill_id {
                    // M17/F30g: предметные кулдауны — с полной
                    // длительностью для прогресс-бара кнопки.
                    if skill.starts_with("item:") {
                        if let Some(total) = cd.duration_ms {
                            self.item_cooldowns
                                .insert(skill, (cd.until_epoch * 1000, total));
                            return vec![];
                        }
                    }
                    self.combat_cooldowns.insert(skill, cd.until_epoch);
                }
                vec![]
            }
            names::EVT_COMBAT_HIT => {
                let Ok(hit) = serde_json::from_value::<CombatHit>(payload) else {
                    return vec![];
                };
                let mine = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.char.id == hit.source);
                self.push_log(
                    if mine {
                        LogKind::CombatOut
                    } else {
                        LogKind::CombatIn
                    },
                    hit.text,
                );
                vec![]
            }
            _ => vec![],
        }
    }

    fn reduce_mob(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_MOB_ATTACK => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobAttack>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        entity.next_attack_at = Some(ev.next_attack_at);
                        entity.attack_interval = Some(ev.interval_ms);
                    }
                }
                vec![]
            }
            names::EVT_MOB_PHASE => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobPhase>(payload) else {
                    return vec![];
                };
                let mut mob_name = ev.entity_id.clone();
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        mob_name = entity.name.clone();
                        let casting = entity.boss.as_ref().and_then(|b| b.casting.clone());
                        entity.boss = Some(ld2_protocol::types::BossState {
                            phase: ev.phase,
                            name: ev.name.clone(),
                            glyph: ev.glyph.clone(),
                            casting,
                        });
                    }
                }
                self.push_log(
                    LogKind::System,
                    format!("{mob_name} входит в фазу: {}", ev.name),
                );
                vec![]
            }
            names::EVT_MOB_CAST => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobCast>(payload) else {
                    return vec![];
                };
                let mut boss_name = ev.entity_id.clone();
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        boss_name = entity.name.clone();
                        if let Some(boss) = entity.boss.as_mut() {
                            boss.casting = Some(ev.name.clone());
                        }
                    }
                }
                self.push_log(
                    LogKind::CombatIn,
                    format!("{boss_name} готовит: {}", ev.name),
                );
                vec![]
            }
            names::EVT_MOB_SKILL => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobSkill>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        if let Some(boss) = entity.boss.as_mut() {
                            boss.casting = None;
                        }
                    }
                }
                self.push_log(
                    LogKind::CombatIn,
                    format!("{} использует {}", ev.source_name, ev.name),
                );
                vec![]
            }
            _ => vec![],
        }
    }

    fn reduce_effects(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_MAGIC_CAST => {
                let Ok(cast) = serde_json::from_value::<MagicCast>(payload) else {
                    return vec![];
                };
                // D4: свой каст тратит ману — бар обновляем по mp_left (§8);
                // заряды приходят патчем evt/item/state.
                if cast.ok {
                    if let Some(mp_left) = cast.mp_left {
                        if let Some(snap) = self.snapshot.as_mut() {
                            if snap.char.id == cast.by {
                                snap.char.mp.cur = mp_left as i64;
                            }
                        }
                    }
                }
                let text = if cast.ok {
                    format!("{}: {} ({:?})", cast.by_name, cast.spell, cast.what)
                } else {
                    format!("{}: {} — сбой", cast.by_name, cast.spell)
                };
                self.push_log(LogKind::Magic, text);
                vec![]
            }
            names::EVT_HARVEST_RESULT => {
                // Снятие шкур (M22): чистим бар, пишем в журнал.
                let Ok(result) =
                    serde_json::from_value::<ld2_protocol::evt::HarvestResult>(payload)
                else {
                    return vec![];
                };
                // M24: бар гаснет только по итогу СВОЕГО трупа —
                // при переключении итог прерванного действия чужого
                // трупа новый бар не гасит.
                if self
                    .harvest
                    .as_ref()
                    .is_some_and(|h| h.entity_id == result.entity_id)
                {
                    self.harvest = None;
                }
                if result.ok {
                    // M23: в лут трупа добавились шкуры/мясо — окно
                    // подятия должно увидеть их без переоткрытия.
                    self.pickup_dirty = true;
                    // M39: сбор ресурса уже отчитался строками предметов
                    // («Вы получили…» / «Найден самоцвет…») — без дубля.
                    if result.action == "skin" {
                        self.push_log(
                            LogKind::Loot,
                            format!("Вы сняли шкуру с трупа {}", result.name),
                        );
                    } else if result.action != "gather" {
                        self.push_log(LogKind::Loot, format!("Вы освежевали труп {}", result.name));
                    }
                } else if result.action == "gather" {
                    // M39: сбор прерван (урон/жила исчезла/нет места)
                    // или не повезло с шансом — сервер шлёт готовую
                    // строку «Не удалось добыть руду: …».
                    let text = result
                        .text
                        .clone()
                        .unwrap_or_else(|| format!("Сбор прерван: {}", result.name));
                    self.push_log(LogKind::Loot, text);
                } else if result.ruined {
                    // M22: разделка прервана взаимодействием с предметом.
                    self.push_log(LogKind::Loot, "Шкура испорчена".to_string());
                }
                vec![]
            }
            names::EVT_CHAR_AFFLICT => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::CharAfflict>(payload)
                else {
                    return vec![];
                };
                if ev.kind == "stun" {
                    self.stun = Some(StunView {
                        title: ev.title,
                        until_ms: ev.until_ms,
                        started_ms: local_epoch_ms(),
                    });
                } else {
                    let secs = ((ev.until_ms - local_epoch_ms()).max(0) + 999) / 1000;
                    self.push_log(LogKind::CombatIn, format!("{} — {} с", ev.title, secs));
                }
                vec![]
            }
            _ => vec![],
        }
    }
}

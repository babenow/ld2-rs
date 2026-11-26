//! Редьюсеры событий мира (RF4d): локация, сущности, опыт/статы/баффы,
//! смерть и повышение уровня. Вынесено из `apply_event` (§7), тела веток
//! перенесены как есть.

use super::super::*;

impl AppStore {
    /// Диспетчер мировых событий: локация, сущности, персонаж.
    pub(crate) fn reduce_world(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Vec<Effect> {
        match msg_type {
            "evt/location/changed" => self.reduce_location(payload),
            m if m.starts_with("evt/entity/") => self.reduce_entity(m, payload),
            m if m.starts_with("evt/char/") => self.reduce_char(m, payload),
            _ => vec![],
        }
    }

    fn reduce_location(&mut self, payload: serde_json::Value) -> Vec<Effect> {
        let Ok(changed) = serde_json::from_value::<LocationChanged>(payload) else {
            return vec![];
        };
        let pvp = changed.location.flags.iter().any(|f| f == "pvp");
        let active: Vec<String> = {
            let now = local_epoch() + self.time_offset;
            self.snapshot
                .as_ref()
                .map(|s| {
                    filter_active_statuses(&s.char.crime.statuses, now)
                        .iter()
                        .map(|s| s.kind.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        // Смена локации — новый контекст: старые тосты устарели.
        self.notice = None;
        if pvp && !active.is_empty() {
            // ФР-11: предупреждение при входе в PvP-зону со статусом.
            self.notice = Some(format!("⚠ PvP-зона: активны статусы {}", active.join(", ")));
        }
        self.push_visited(&changed.location);
        if let Some(snap) = self.snapshot.as_mut() {
            snap.location = changed.location;
        }
        self.entity_detail = None;
        self.pickup = None;
        self.world_error = None;
        // M29 (заказчик): строка «Вы вошли в …» не нужна — переход и так
        // виден по смене заголовка и событиям соседей.
        vec![]
    }

    fn reduce_entity(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            "evt/entity/spawn" => {
                let Ok(spawn) = serde_json::from_value::<EntitySpawn>(payload) else {
                    return vec![];
                };
                let mut added = None;
                if let Some(snap) = self.snapshot.as_mut() {
                    if snap.location.id == spawn.location_id
                        && !snap
                            .location
                            .entities
                            .iter()
                            .any(|e| e.id == spawn.entity.id)
                    {
                        added = Some((spawn.entity.name.clone(), spawn.text.clone()));
                        snap.location.entities.push(spawn.entity);
                    }
                }
                if let Some((name, text)) = added {
                    // M29 (заказчик): приход соседа — готовая строка
                    // сервера («пришла Сара»); без неё — шаблон.
                    match text.as_deref().filter(|l| !l.is_empty()) {
                        Some(line) => self.push_log(LogKind::Location, line),
                        None => self.push_log(LogKind::System, format!("{name} появляется")),
                    }
                }
                vec![]
            }
            "evt/entity/move" => {
                let Ok(moved) = serde_json::from_value::<EntityMove>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == moved.entity_id)
                    {
                        if let Some(hp) = moved.patch.get("hp") {
                            if let Ok(hp) = serde_json::from_value::<Hp>(hp.clone()) {
                                entity.hp = Some(hp);
                            }
                        }
                    }
                }
                vec![]
            }
            "evt/entity/hp" => {
                let Ok(update) = serde_json::from_value::<EntityHp>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == update.entity_id)
                    {
                        entity.hp = Some(update.hp);
                    }
                }
                vec![]
            }
            "evt/entity/despawn" => {
                let Ok(gone) = serde_json::from_value::<EntityDespawn>(payload) else {
                    return vec![];
                };
                let mut name = None;
                let mut qty = 1;
                let mut upgrade = 0;
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(idx) = snap
                        .location
                        .entities
                        .iter()
                        .position(|e| e.id == gone.entity_id)
                    {
                        let ent = snap.location.entities.remove(idx);
                        name = Some(ent.name);
                        qty = ent
                            .extra
                            .get("qty")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(1)
                            .max(1);
                        upgrade = ent
                            .extra
                            .get("upgrade")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                    }
                }
                let upg = if upgrade > 0 {
                    format!(" +{upgrade}")
                } else {
                    String::new()
                };
                // M29 (заказчик): сервер прислал готовую строку ухода
                // игрока («{имя} ушел — {выход}») — она важнее шаблонов.
                // Пишем и когда строки в снапшоте нет (запоздалое событие,
                // сосед ушёл между снапшотами) — журнал не теряет факт.
                if gone.text.as_deref().is_some_and(|l| !l.is_empty()) {
                    self.push_log(LogKind::Location, gone.text.unwrap().to_string());
                } else if let Some(name) = name {
                    // R-dnd (заказчик 2026-11-21): подбор с земли —
                    // своя строка «Вы подняли…», чужая — «{имя}
                    // подобрал(а)…»; без picked_by — TTL «исчезает».
                    match gone.picked_by.as_deref() {
                        Some(by) => {
                            let mine = self.snapshot.as_ref().is_some_and(|s| s.char.name == by);
                            if mine {
                                self.push_log(
                                    LogKind::Loot,
                                    format!("Вы подняли {qty} {name}{upg}"),
                                );
                            } else {
                                let verb = if gone.picked_gender.as_deref() == Some("female") {
                                    "подобрала"
                                } else {
                                    "подобрал"
                                };
                                self.push_log(
                                    LogKind::Loot,
                                    format!("{by} {verb} {qty} {name}{upg}"),
                                );
                            }
                        }
                        None => {
                            self.push_log(LogKind::System, format!("{name} исчезает"));
                        }
                    }
                }
                // ФР-05: авто-сброс цели при исчезновении.
                if self.target.as_deref() == Some(gone.entity_id.as_str()) {
                    self.target = None;
                }
                vec![]
            }
            _ => vec![],
        }
    }

    fn reduce_char(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            "evt/char/level_up" => {
                // M22: новый уровень — журнал: себе «Вы получили…»,
                // остальным в локации «{имя} получил(а)…».
                let Ok(up) = serde_json::from_value::<ld2_protocol::evt::CharLevelUp>(payload)
                else {
                    return vec![];
                };
                let mine = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.char.id == up.char_id);
                if mine {
                    self.push_log(
                        LogKind::Xp,
                        format!("Вы получили новый уровень! Уровень персонажа: {}", up.level),
                    );
                } else {
                    let verb = if up.gender == "female" {
                        "получила"
                    } else {
                        "получил"
                    };
                    self.push_log(LogKind::Xp, format!("{} {} новый уровень!", up.name, verb));
                }
                vec![]
            }
            "evt/char/xp" => {
                let Ok(xp) = serde_json::from_value::<CharXp>(payload) else {
                    return vec![];
                };
                // M22: опыт персонажа — отдельная строка в журнале.
                if xp.amount > 0 {
                    self.push_log(LogKind::Xp, format!("Опыт +{}", xp.amount));
                }
                // Обновляем лист: навык и остаток пула (ФР-04).
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(pool) = xp.pool_left {
                        snap.char.xp_pool = pool;
                    }
                    if let Some(skill) = snap.char.skills.iter_mut().find(|s| s.id == xp.skill_id) {
                        skill.xp = xp.skill_xp;
                        if let Some(rank) = xp.rank {
                            skill.rank = rank;
                        }
                        if let Some(next) = xp.xp_to_next {
                            skill.xp_to_next = next;
                        }
                    }
                }
                let text = if xp.amount < 0 {
                    // M18/F44: повышение ранга у тренера — очки пула
                    // списаны (amount отрицательный), шкала обнулена.
                    match xp.rank {
                        Some(rank) => {
                            format!("{} — ранг {rank}! (−{} очков)", xp.skill_id, -xp.amount)
                        }
                        None => format!("{} (−{} очков)", xp.skill_id, -xp.amount),
                    }
                } else {
                    match xp.rank {
                        Some(rank) => format!("+{} XP → {} — ранг {rank}!", xp.amount, xp.skill_id),
                        None => format!("+{} XP → {}", xp.amount, xp.skill_id),
                    }
                };
                self.push_log(LogKind::Xp, text);
                vec![]
            }
            "evt/char/stats" => {
                // Патч ресурсов/атрибутов (respawn, зелья) — ФР-06/ФР-07.
                let Ok(stats) = serde_json::from_value::<CharStats>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(hp) = stats.patch.get("hp") {
                        if let Ok(hp) = serde_json::from_value::<Hp>(hp.clone()) {
                            snap.char.hp = hp;
                        }
                    }
                    if let Some(mp) = stats.patch.get("mp") {
                        if let Ok(mp) = serde_json::from_value::<Hp>(mp.clone()) {
                            snap.char.mp = mp;
                        }
                    }
                    if let Some(attrs) = stats.patch.get("attrs").and_then(|v| v.as_object()) {
                        for (key, value) in attrs {
                            if let Some(n) = value.as_i64() {
                                snap.char.attrs.insert(key.clone(), n);
                            }
                        }
                    }
                }
                vec![]
            }
            "evt/char/death" => {
                // ФР-06: сводка утраченного — модалка, закрывается подтверждением.
                // M18: в журнал НЕ выводим — утраченное видно в самой
                // модалке (полный список, со скроллом).
                let Ok(death) = serde_json::from_value::<CharDeath>(payload) else {
                    return vec![];
                };
                self.death = Some(death);
                self.target = None;
                vec![Effect::Notify(NotifyKind::Death)]
            }
            "evt/char/buffs" => {
                // M18/F58d (ADR-012 §6): полная замена списка баффов рун.
                let Ok(update) = serde_json::from_value::<ld2_protocol::evt::CharBuffs>(payload)
                else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.buffs = update.buffs.clone();
                }
                if !update.buffs.is_empty() {
                    let titles: Vec<&str> = update.buffs.iter().map(|b| b.title.as_str()).collect();
                    self.push_log(
                        LogKind::System,
                        format!("Бафф: {} ×{}", titles.join(", "), {
                            update.buffs.iter().map(|b| b.stacks).max().unwrap_or(1)
                        }),
                    );
                }
                vec![]
            }
            _ => vec![],
        }
    }
}

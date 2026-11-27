//! RF1g: бой и навыки (ФР-05/ФР-06) — атака/стоп/побег, применение
//! навыка и обучение у тренера (ADR-009), оплата обучения в диалоге.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::names;

use ld2_protocol::cmd::{CombatAttack, SkillTrain, SkillUse};
use ld2_protocol::envelope::{Ack, Envelope};
use ld2_protocol::evt::{CharStats, CharXp, CombatCooldown, CombatHit, CombatKind, EntityHp};
use ld2_protocol::types::Hp;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::{out_ack, out_event, push_evt, Out, Session};
use crate::db;
use crate::trainers;

impl Session {
    /// `cmd/combat.attack` (§7): начать/сменить бой с мобом текущей локации
    /// (тик-луп мира наносит удары, M12.5).
    pub(super) async fn combat_attack(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CombatAttack>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let location = self.current_location(&char_id).await;
        let now = crate::time::now_ms();
        let started = {
            let mut world = self.state.world.lock().expect("мир");
            world.start_combat(&char_id, &location, &cmd.target_id, now)
        };
        if !started {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/combat.stop` (§7): выйти из боя.
    pub(super) async fn combat_stop(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        {
            let mut world = self.state.world.lock().expect("мир");
            world.end_combat(&char_id);
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/combat.flee` (§7): шанс побега из конфига `[gameplay.combat]`;
    /// провал → `E_RATE`-стиль `E_COOLDOWN` с `retry_after_s`.
    pub(super) async fn combat_flee(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let in_combat = {
            let world = self.state.world.lock().expect("мир");
            world.in_combat(&char_id)
        };
        if !in_combat {
            return vec![Self::err(seq, ErrorCode::NotInLocation, "вы не в бою")];
        }
        let cfg = &self.state.config.gameplay.combat;
        // R7: CSPRNG — две последние цифры времени полностью обходили
        // шанс побега (спам побегом каждый мс).
        let fled = crate::world::roll_chance(cfg.flee_chance);
        {
            let mut world = self.state.world.lock().expect("мир");
            if fled {
                world.end_combat(&char_id);
            }
        }
        if fled {
            vec![Self::ack_payload(seq, &serde_json::json!({}))]
        } else {
            let retry = (cfg.attack_interval_s.ceil() as u64).max(1);
            let mut ack = Ack::error(seq, None, &ErrorCode::Cooldown, "не удалось сбежать");
            if let Some(err) = ack.error.as_mut() {
                err.retry_after_s = Some(retry);
            }
            vec![out_ack(ack)]
        }
    }

    /// `cmd/skill.use` (§7, skills.md §4, M14): активная способность.
    /// v1 — `power_strike`: ×`damage_mult` от `base_damage` по цели-мобу,
    /// серверный кулдаун (`skills.active.<id>.cooldown_s`) → `E_COOLDOWN`,
    /// гейт навыка → `E_REQUIREMENT_ATTR`, нет цели → `E_TARGET_GONE`.
    pub(super) async fn skill_use(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SkillUse>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Some(active) = self.state.config.skills.active.get(&cmd.skill_id).cloned() else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                format!("неизвестная способность: {}", cmd.skill_id),
            )];
        };
        // Локация до заимствования pool (current_location требует &mut self).
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        let now_s = crate::time::now_epoch();

        // Гейт по навыку (§4: открывается рангом).
        if let Some(req) = active.req_skill.as_deref() {
            let rank = db::skills::learned_one(pool, &char_id, req)
                .await
                .ok()
                .flatten()
                .map(|(_, r)| r)
                .unwrap_or(0);
            if rank < active.req_rank as i32 {
                return vec![Self::err(
                    seq,
                    ErrorCode::RequirementAttr,
                    format!(
                        "нужен навык «{req}» ранг {} (сейчас {rank})",
                        active.req_rank
                    ),
                )];
            }
        }

        // Цель: моб в локации персонажа (одна цель — §4).
        let Some(target) = cmd.target_id.as_deref() else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не указана")];
        };
        {
            let world = self.state.world.lock().expect("мир");
            let alive = world.mob_location(target).as_deref() == Some(location.as_str());
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        // Серверный кулдаун (§4/E_COOLDOWN + retry_after_s) — после гейта
        // и цели: запрос валидируется целиком, потом — rate-limit.
        {
            let world = self.state.world.lock().expect("мир");
            if let Some(left) = world.ability_cooldown_left(&char_id, &cmd.skill_id, now_s) {
                let mut ack = Ack::error(
                    seq,
                    None,
                    &ErrorCode::Cooldown,
                    "способность перезаряжается",
                );
                if let Some(err) = ack.error.as_mut() {
                    err.retry_after_s = Some(left.max(1) as u64);
                }
                return vec![out_ack(ack)];
            }
        }

        // M17/F30e: ресурс класса — стоимость активной способности
        // (ярость воина / энергия лучника / мана мага; 0 — бесплатно)
        // и профиль для прироста ярости за урон.
        let Some(crow) = (match db::characters::get(pool, &char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("skill.use (ресурс): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        let profile = crow.profile.as_str();
        let res_cfg = &self.state.config.gameplay.resource;
        let res_max = crate::systems::resource_max(profile, res_cfg, i64::from(crow.mp_max));
        let mut res_cur = i64::from(crow.mp_cur).min(res_max);
        let mut resource_spent = false;
        if active.cost > 0 {
            let cost = i64::from(active.cost);
            if res_cur < cost {
                return vec![Self::err(
                    seq,
                    ErrorCode::NotEnoughMp,
                    format!(
                        "не хватает {}: {res_cur} < {cost}",
                        crate::systems::resource_name(profile)
                    ),
                )];
            }
            res_cur -= cost;
            if let Err(e) = db::characters::set_mp(pool, &char_id, res_cur as i32).await {
                warn!("skill.use (списание ресурса): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            resource_spent = true;
        }

        // M32: урон способности — бросок урона листа × множитель;
        // точность/уклон/парирование/броня моба — как у авто-атаки.
        let (damage, miss) = {
            let mut world = self.state.world.lock().expect("мир");
            let combat = world
                .char_combat(&char_id)
                .unwrap_or(crate::world::CharCombat {
                    damage_min: self.state.config.gameplay.combat.base_damage,
                    damage_max: self.state.config.gameplay.combat.base_damage,
                    accuracy: 80,
                    dodge: 0,
                    parry: 0,
                    armor: 0,
                });
            let (mob_armor, mob_dodge, mob_parry, _, _) = world.mob_defense(target);
            let rolls = (world.roll_pct(), world.roll01(), world.roll_pct());
            match crate::systems::ability_damage(
                combat.accuracy,
                mob_dodge,
                combat.damage_min,
                combat.damage_max,
                mob_armor,
                mob_parry,
                active.damage_mult,
                rolls.0,
                rolls.1,
                rolls.2,
            ) {
                Some(dmg) => (dmg, false),
                None => (0, true),
            }
        };
        // M17/F30e: ярость воина за нанесённый урон (15 % — конфиг).
        let rage = crate::systems::rage_gain(res_cfg, profile, damage);

        // Кулдаун фиксируем ДО удара (защита от двойного нажатия в одном тике).
        let until = now_s + active.cooldown_s.ceil() as i64;
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_ability_cooldown(&char_id, &cmd.skill_id, until);
        }

        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        mine.push(out_event(
            names::EVT_COMBAT_COOLDOWN,
            None,
            serde_json::to_value(CombatCooldown {
                skill_id: Some(cmd.skill_id.clone()),
                until_epoch: until,
                // M17/F30g: полная длительность — прогресс-бар клиента.
                duration_ms: Some((active.cooldown_s * 1000.0).round() as i64),
            })
            .unwrap_or_default(),
        ));
        // M17/F30e: списанный ресурс — сразу клиенту (патч mp).
        if resource_spent {
            let mut patch = serde_json::Map::new();
            patch.insert(
                "mp".into(),
                serde_json::json!({ "cur": res_cur, "max": res_max }),
            );
            mine.push(out_event(
                names::EVT_CHAR_STATS,
                None,
                serde_json::to_value(CharStats { patch }).unwrap_or_default(),
            ));
        }

        // M17/F30f: имя способности из контента — в логе без сырых id.
        let skill_name = match db::skills::content_skill(pool, &cmd.skill_id).await {
            Ok(Some(row)) => row.name,
            _ => cmd.skill_id.clone(),
        };

        // Урон цели: hit/hp → при смерти: despawn/труп/бой окончен + XP.
        let mut others: Vec<(String, String)> = Vec::new();
        let mut xp_work: Option<crate::tick::KillXp> = None;
        let mut hit_done = false;
        {
            let mut world = self.state.world.lock().expect("мир");
            if miss {
                let mob_name = world
                    .mob(target)
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                let hit = CombatHit {
                    source: char_id.clone(),
                    source_name: by_name.clone(),
                    target: target.to_string(),
                    target_name: mob_name,
                    kind: CombatKind::Skill,
                    amount: 0,
                    crit: false,
                    miss: true,
                    text: format!("«{skill_name}»: промах"),
                    extra: Default::default(),
                };
                for addr in world.chars_in_location(&location) {
                    push_evt(&mut others, &addr, names::EVT_COMBAT_HIT, &hit);
                }
            } else if let Some(hp) = world.damage_mob(target, damage) {
                hit_done = true;
                let mob_name = world
                    .mob(target)
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                let mob_max = world.mob_max_hp(target);
                let hit = CombatHit {
                    source: char_id.clone(),
                    source_name: by_name.clone(),
                    target: target.to_string(),
                    target_name: mob_name,
                    kind: CombatKind::Skill,
                    amount: damage,
                    crit: false,
                    miss: false,
                    text: format!("«{skill_name}»: **−{damage}**"),
                    extra: Default::default(),
                };
                for addr in world.chars_in_location(&location) {
                    push_evt(&mut others, &addr, names::EVT_COMBAT_HIT, &hit);
                    push_evt(
                        &mut others,
                        &addr,
                        names::EVT_ENTITY_HP,
                        &EntityHp {
                            entity_id: target.to_string(),
                            hp: Hp {
                                cur: hp,
                                max: mob_max,
                            },
                        },
                    );
                }
                if hp == 0 {
                    let skills_cfg = &self.state.config.skills;
                    xp_work = crate::tick::mob_kill_effects(
                        &mut world,
                        &mut others,
                        target,
                        &location,
                        &char_id,
                        skills_cfg,
                    );
                }
            }
        }
        // M17/F30e: прирост ярости за нанесённый урон (воин) — после
        // удара, с патчем клиенту.
        if rage > 0 && hit_done {
            let new_rage = (res_cur + rage).min(res_max);
            if new_rage > res_cur {
                match db::characters::set_mp(pool, &char_id, new_rage as i32).await {
                    Ok(()) => {
                        let mut patch = serde_json::Map::new();
                        patch.insert(
                            "mp".into(),
                            serde_json::json!({ "cur": new_rage, "max": res_max }),
                        );
                        mine.push(out_event(
                            names::EVT_CHAR_STATS,
                            None,
                            serde_json::to_value(CharStats { patch }).unwrap_or_default(),
                        ));
                    }
                    Err(e) => warn!("skill.use (ярость {char_id}): {e}"),
                }
            }
        }
        for (addr, text) in others {
            if text.is_empty() {
                continue;
            }
            if addr == char_id {
                mine.push(Out::Text(text));
            } else {
                let world = self.state.world.lock().expect("мир");
                world.send_to_char(&addr, text);
            }
        }
        if let Some(work) = &xp_work {
            if let Some(text) = crate::tick::grant_kill_xp(&self.state, work).await {
                mine.push(Out::Text(text));
            }
            if let Some(target) = &work.target {
                for (_, text) in
                    crate::tick::quest_kill_progress(&self.state, &char_id, target).await
                {
                    if !text.is_empty() {
                        mine.push(Out::Text(text));
                    }
                }
            }
        }
        mine
    }

    /// `cmd/skill.train` (M18/F44, ADR-009): повышение ранга навыка у
    /// тренера **из диалога**. Условия: тренер по навыку в локации (M14,
    /// `E_NO_TRAINER`), у NPC есть запись обучения, **шкала навыка
    /// заполнена на 100 %** (`E_SKILL_NOT_READY`), хватает очков опыта
    /// (`E_XP_POOL`) и платы из контента (`E_INGREDIENTS`). Потолок ранга
    /// и гейт атрибутов — как у прежнего `char.xp_invest` (удалён: он
    /// обходил гейт 100 %).
    pub(super) async fn skill_train(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SkillTrain>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Локация до заимствования pool (current_location требует &mut self).
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        // M14: обучение только у тренера по навыку в локации — маркер
        // контента (`locations.json → trainer.skills`) служит гейтом;
        // цена при этом берётся из диалога (ADR-009).
        let trainer = match trainers::trainer_price(
            pool,
            &location,
            trainers::TrainerKind::Skill,
            &cmd.skill_id,
        )
        .await
        {
            Ok(price) => price,
            Err(e) => {
                warn!("skill.train (тренер): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if trainer.is_none() {
            return vec![Self::err(
                seq,
                ErrorCode::NoTrainer,
                "здесь нет тренера по этому навыку",
            )];
        }
        // NPC диалога несёт список навыков и плату (ADR-009).
        let entity = match self.require_npc(seq, &cmd.entity_id).await {
            Ok(entity) => entity,
            Err(out) => return vec![out],
        };
        let content = self.state.content();
        let entry = match content
            .dialogs
            .get(&entity.id)
            .and_then(|def| def.training.iter().find(|t| t.skill == cmd.skill_id))
        {
            Some(entry) => entry.clone(),
            None => {
                return vec![Self::err(
                    seq,
                    ErrorCode::NoTrainer,
                    "этот NPC не обучает этому навыку",
                )];
            }
        };
        // Навык есть в контенте (группа нужна для потолка по уровню).
        let content_skill = match db::skills::content_skill(pool, &cmd.skill_id).await {
            Ok(Some(skill)) => skill,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::XpPool, "нет такого навыка")];
            }
            Err(e) => {
                warn!("skill.train (навык): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // Прогресс: строка создаётся использованием навыка (тик боя/
        // крафта); без неё шкала пустая → отказ до оплаты.
        let (xp, rank) = match db::skills::learned(pool, &char_id).await {
            Ok(rows) => rows
                .into_iter()
                .find(|s| s.skill_id == cmd.skill_id)
                .map(|s| (s.xp, s.rank))
                .unwrap_or((0, 0)),
            Err(e) => {
                warn!("skill.train (прогресс): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let cfg = &self.state.config.skills;
        if rank >= cfg.rank_max as i32 {
            return vec![Self::err(
                seq,
                ErrorCode::RankCap,
                "достигнут потолок ранга",
            )];
        }
        let next = rank + 1;
        // M31: потолок ранга по уровню персонажа — не более 2 за уровень
        // для боевых/защитных/магических навыков (заказчик). Проверяем до
        // шкалы 100 %: у потолка она может быть полной, но ранг не растёт.
        let total_xp = match db::characters::get(pool, &char_id).await {
            Ok(Some(row)) => row.total_xp,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::Internal, "персонаж не найден")];
            }
            Err(e) => {
                warn!("skill.train (уровень): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let (char_level, ..) =
            crate::systems::level_for_total(total_xp, &self.state.config.gameplay.level);
        let rank_cap = crate::systems::skill_rank_cap(cfg, &content_skill.skill_group, char_level);
        if next as u32 > rank_cap {
            return vec![Self::err(
                seq,
                ErrorCode::RankCap,
                format!(
                    "потолок ранга по уровню: {} за уровень (уровень {char_level} → максимум {rank_cap})",
                    cfg.ranks_per_level
                ),
            )];
        }
        let need = crate::systems::xp_to_next(rank.max(0) as u32, cfg);
        // M18/F44: повышение ранга — только при полной шкале (100 %).
        if need > 0 && xp < need {
            let percent = (xp * 100 / need.max(1)).clamp(0, 100);
            return vec![Self::err(
                seq,
                ErrorCode::SkillNotReady,
                format!("шкала навыка заполнена на {percent} % — нужно 100 %"),
            )];
        }
        // Гейт атрибутов (§2.2 skills.md) — как у прежнего вложения.
        if let Some(gate) = cfg.gates.iter().find(|g| g.rank as i32 == next) {
            let max_attr = self
                .char
                .as_ref()
                .map(|c| c.attrs.values().max().copied().unwrap_or(0))
                .unwrap_or(0);
            if max_attr < gate.attr {
                return vec![Self::err(
                    seq,
                    ErrorCode::RequirementAttr,
                    format!(
                        "ранг {next} требует атрибут {} (максимум {max_attr})",
                        gate.attr
                    ),
                )];
            }
        }
        // Плата очками опыта (пул квестов).
        let pool_now = match db::skills::xp_pool(pool, &char_id).await {
            Ok(p) => p,
            Err(e) => {
                warn!("skill.train (пул): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if pool_now < entry.points {
            return vec![Self::err(
                seq,
                ErrorCode::XpPool,
                format!("не хватает очков опыта: {}", entry.points),
            )];
        }
        // Плата тренеру (золото + предметы из диалога) — проверки до
        // транзакции (паттерн M14/F5).
        if entry.gold > 0 {
            match db::items::count_by_type(pool, &char_id, db::items::MONEY_TYPE).await {
                Ok(have) if have >= entry.gold => {}
                Ok(have) => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("не хватает золота: {} (есть {have})", entry.gold),
                    )];
                }
                Err(e) => {
                    warn!("skill.train (монеты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }
        if !entry.items.is_empty() {
            let inventory = match db::items::inventory(pool, &char_id).await {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("skill.train (инвентарь): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            for item in &entry.items {
                let have: i64 = inventory
                    .iter()
                    .filter(|row| row.def_id == item.def_id)
                    .map(|row| i64::from(row.qty))
                    .sum();
                if have < item.qty {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("нужен предмет: {} ×{} (есть {have})", item.def_id, item.qty),
                    )];
                }
            }
        }
        // R6: списание платы и вклад очков — одной транзакцией; обрыв
        // списания больше не теряет уже списанное (откат всего).
        let (money_events, pool_left) = {
            let mut tx = match pool.begin().await {
                Ok(tx) => tx,
                Err(e) => {
                    warn!("skill.train (tx): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let events = match self
                .pay_dialog_fee(&mut tx, seq, &char_id, entry.gold, &entry.items)
                .await
            {
                Ok(events) => events,
                Err(e) => return vec![e],
            };
            let pool_left = pool_now - entry.points;
            // Ранг +1, шкала обнуляется (заполняется использованием заново).
            if let Err(e) =
                db::skills::invest_core(&mut tx, &char_id, &cmd.skill_id, 0, next, pool_left).await
            {
                warn!("skill.train (вклад): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            if let Err(e) = tx.commit().await {
                warn!("skill.train (commit): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            (events, pool_left)
        };
        // M32: новый ранг навыка → пересчёт урона/защиты в кэше мира.
        self.refresh_char_hits_as(&char_id).await;
        let mut outs = vec![
            Self::ack_payload(
                seq,
                &serde_json::json!({
                    "rank": next.max(0) as u32,
                    "xp": 0,
                    "xp_to_next": crate::systems::xp_to_next(next.max(0) as u32, cfg),
                    "pool_left": pool_left,
                }),
            ),
            out_event(
                names::EVT_CHAR_XP,
                None,
                serde_json::to_value(CharXp {
                    skill_id: cmd.skill_id,
                    // Отрицательное amount — очки пула списаны на ранг.
                    amount: -entry.points,
                    skill_xp: 0,
                    rank: Some(next.max(0) as u32),
                    xp_to_next: Some(crate::systems::xp_to_next(next.max(0) as u32, cfg)),
                    pool_left: Some(pool_left),
                })
                .unwrap_or_default(),
            ),
        ];
        outs.extend(money_events);
        outs
    }

    /// M18/F44: плата за обучение из контента диалога (ADR-009) — золото
    /// монетами, предметы по `def_id`. R6: списания — на внешней
    /// транзакции (проверки сделаны в skill_train до begin; паттерн
    /// `pay_trainer` M14/F5).
    pub(super) async fn pay_dialog_fee(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        seq: u32,
        char_id: &str,
        gold: i64,
        items: &[crate::dialogs::TrainingItem],
    ) -> Result<Vec<Out>, Out> {
        let mut events = Vec::new();
        if gold > 0 {
            match db::items::consume_money_tx(tx, char_id, gold).await {
                Ok(Some(changes)) => events.extend(Self::money_events(changes)),
                Ok(None) => {
                    return Err(Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("не хватает золота: {gold}"),
                    ));
                }
                Err(e) => {
                    warn!("skill.train (списание золота): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            }
        }
        for item in items {
            match db::items::consume_by_def_tx(tx, char_id, &item.def_id, item.qty).await {
                Ok(Some(changes)) => events.extend(Self::money_events(changes)),
                Ok(None) => {
                    return Err(Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("нужен предмет: {} ×{}", item.def_id, item.qty),
                    ));
                }
                Err(e) => {
                    warn!("skill.train (списание {}): {e}", item.def_id);
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            }
        }
        Ok(events)
    }

    /// Оплата тренеру (M14): сначала **проверки** (золото и предметы),
    /// потом списания — иначе отказ «нет предмета» съедал бы уже
    /// уплаченное золото. Нехватка → `E_INGREDIENTS` (гейт тренера и
    /// остальные проверки выполнены до вызова).
    pub(super) async fn pay_trainer(
        &self,
        seq: u32,
        char_id: &str,
        price: &trainers::TrainerPrice,
        gold: i64,
    ) -> Result<Vec<Out>, Out> {
        let pool = &self.state.pool;

        // 1) Проверки.
        if gold > 0 {
            // F29a: баланс — сумма стаков монет (type=money).
            let have = match db::items::count_by_type(pool, char_id, db::items::MONEY_TYPE).await {
                Ok(have) => have,
                Err(e) => {
                    warn!("тренер (монеты): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            };
            if have < gold {
                return Err(Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("не хватает золота: {gold}"),
                ));
            }
        }
        for (item_type, need) in &price.items {
            let have = match db::items::count_by_type(pool, char_id, item_type).await {
                Ok(have) => have,
                Err(e) => {
                    warn!("тренер (предмет {item_type}): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            };
            if have < *need {
                return Err(Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("нужен предмет: {item_type} ×{need} (есть {have})"),
                ));
            }
        }

        // 2) Списания (все проверки прошли); F29a: монеты — стаки,
        // изменения уходят событиями в outs вызывающего.
        let mut events = Vec::new();
        if gold > 0 {
            match db::items::consume_money(pool, char_id, gold).await {
                Ok(Some(changes)) => events.extend(Self::money_events(changes)),
                Ok(None) => {
                    return Err(Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("не хватает золота: {gold}"),
                    ));
                }
                Err(e) => {
                    warn!("тренер (монеты): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            }
        }
        for (item_type, need) in &price.items {
            match db::items::consume_by_type(pool, char_id, item_type, *need).await {
                Ok(true) => {}
                Ok(false) => {
                    return Err(Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("нужен предмет: {item_type} ×{need}"),
                    ));
                }
                Err(e) => {
                    warn!("тренер (списание {item_type}): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            }
        }
        Ok(events)
    }
}

//! RF1j: магия (M19) — каст руны и свитка: проверка требований, расход
//! предмета, эффекты заклинания и события мира.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::names;

use ld2_protocol::cmd::{MagicRuneCast, MagicScrollCast};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{
    CastSource, CombatHit, CombatKind, EntityHp, ItemRemovalReason, ItemRemoved, ItemState,
    MagicCast,
};
use ld2_protocol::types::Hp;
use ld2_protocol::ErrorCode;
use serde_json::Value;
use tracing::warn;

use super::rules::item_snapshot;
use super::{event_payload_text, out_event, refresh_char_runes, Out, Session};
use crate::db;

impl Session {
    /// `cmd/magic.rune_cast` (§7, D4): мана + заряд руны → урон цели
    /// (`stats.damage`), `evt/magic/cast` всем в локации, XP убийства — в
    /// школу (`stats.school`). Ошибки: `E_NO_RUNE_SLOT`, `E_NO_CHARGES`,
    /// `E_NOT_ENOUGH_MP`, `E_TARGET_GONE`.
    pub(super) async fn magic_rune_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MagicRuneCast>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(mut staff) = (match db::items::find(&pool, &cmd.staff_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("rune_cast (поиск посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(staff_def) = (match db::items::def(&pool, &staff.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("rune_cast (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения посоха",
            )];
        };
        let sockets: Vec<Value> = serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        let Some(slot) = sockets.get(cmd.socket_index as usize) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "нет такого сокета")];
        };
        let Some(rune_info) = slot.get("rune").filter(|r| !r.is_null()) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет пуст")];
        };
        let charges = rune_info
            .get("charges")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let rune_def_id = slot
            .get("def_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(rune_def) = (match db::items::def(&pool, &rune_def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("rune_cast (определение руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения руны")];
        };
        // M18/F58c (ADR-012 §4): затрата — поле руны `cost` (деф. 1),
        // зарядов должно хватать на неё целиком.
        let cost_charges = crate::systems::rune_charge_cost(&rune_def.stats);
        if charges < cost_charges {
            return vec![Self::err(seq, ErrorCode::NoCharges, "руна разряжена")];
        }
        // M25 (заказчик): каст с любого предмета с сокетами — панель
        // показывает руны мечей, активация должна работать (раньше
        // «только посох» — E_NO_RUNE_SLOT на кнопке меча).
        if staff_def.sockets == 0 {
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                "у предмета нет сокетов",
            )];
        }
        // M24: класс руны (stats.class) — каст только своим классом
        // (нет поля — любой, §11). После проверки сокетов.
        if let Some(class) = rune_def.stats.get("class").and_then(|v| v.as_str()) {
            let profile = db::characters::get(&pool, &char_id)
                .await
                .ok()
                .flatten()
                .map(|r| r.profile);
            if profile.as_deref() != Some(class) {
                let need = match class {
                    "mage" => "маг",
                    "warrior" => "воин",
                    "archer" => "лучник",
                    other => other,
                };
                return vec![Self::err(
                    seq,
                    ErrorCode::RequirementAttr,
                    format!("руна требует класс: {need}"),
                )];
            }
        }
        // M17/F30g: кулдаун каста руны (cooldown_s, дефолт 1 с).
        if let Some(err) = self.item_cd_gate(seq, &char_id, &rune_def).await {
            return vec![err];
        }

        // Мана (D4: каст тратит ману И заряд).
        let cost = i64::from(self.state.config.gameplay.magic.mana.rune_cost);
        let Some(row) = (match db::characters::get(&pool, &char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("rune_cast (персонаж): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        if i64::from(row.mp_cur) < cost {
            return vec![Self::err(seq, ErrorCode::NotEnoughMp, "не хватает маны")];
        }
        let spell = rune_info
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let damage = rune_def
            .stats
            .get("damage")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let school = rune_def
            .stats
            .get("school")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Цель (опциональна): моб должен быть в локации персонажа.
        let location = self.current_location(&char_id).await;
        if let Some(target) = cmd.target_id.as_deref() {
            let alive = {
                let world = self.state.world.lock().expect("мир");
                world.mob_location(target).as_deref() == Some(location.as_str())
            };
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        // Расход: мана и заряд руны (D27 — сразу в БД).
        let new_mp = (i64::from(row.mp_cur) - cost).max(0) as i32;
        if let Err(e) = db::characters::set_mp(&pool, &char_id, new_mp).await {
            warn!("rune_cast (mp): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let mut sockets: Vec<Value> =
            serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        if let Some(slot) = sockets.get_mut(cmd.socket_index as usize) {
            if let Some(r) = slot.get_mut("rune") {
                r["charges"] = serde_json::json!(charges - cost_charges);
            }
        }
        let sockets_value = Value::Array(sockets);
        if let Err(e) = db::items::set_sockets(&pool, &staff.uid, sockets_value.clone()).await {
            warn!("rune_cast (заряды): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        // M18/F58d: кэш рун — charges изменились.
        refresh_char_runes(&self.state, &char_id).await;
        // M18/F58c: клиенту — патч сокетов (иначе заряды на кнопке
        // отстают до ресинка; сокеты в wire-виде — с `accepts`, §6).
        staff.sockets = sockets_value;
        let mut item_patch = serde_json::Map::new();
        item_patch.insert(
            "sockets".into(),
            serde_json::to_value(
                item_snapshot(&staff, &staff_def)
                    .sockets
                    .unwrap_or_default(),
            )
            .unwrap_or(Value::Null),
        );
        let charge_event = out_event(
            names::EVT_ITEM_STATE,
            None,
            serde_json::to_value(ItemState {
                uid: staff.uid.clone(),
                patch: item_patch,
            })
            .unwrap_or_default(),
        );

        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();

        let cast = MagicCast {
            by: char_id.clone(),
            by_name: by_name.clone(),
            spell: spell.clone(),
            what: CastSource::Rune,
            target: cmd.target_id.clone(),
            charges_left: Some((charges - cost_charges) as i32),
            mp_left: Some(new_mp),
            ok: true,
            extra: Default::default(),
        };
        let cast_json = event_payload_text(names::EVT_MAGIC_CAST, &cast);

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if !matches!(&charge_event, Out::Text(t) if t.is_empty()) {
            mine.push(charge_event);
        }
        if let Some(text) = &cast_json {
            mine.push(Out::Text(text.clone()));
        }

        // cast-событие — всем в локации (свой — уже в `mine`).
        let mut others: Vec<(String, String)> = Vec::new();
        if let Some(text) = &cast_json {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&location) {
                if addr != char_id {
                    others.push((addr, text.clone()));
                }
            }
        }
        let mut xp_work: Option<crate::tick::KillXp> = None;
        if let Some(target) = cmd.target_id.as_deref() {
            let skills_cfg = &self.state.config.skills;
            let mut plan: Vec<(String, String)> = Vec::new();
            {
                let mut world = self.state.world.lock().expect("мир");
                // M32: магическое сопротивление моба снижает урон.
                let damage = crate::systems::after_resist(damage, world.mob_resist(target));
                if let Some(hp) = world.damage_mob(target, damage) {
                    let mob_name = world
                        .mob(target)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    let mob_max = world.mob_max_hp(target);
                    // M17/F30f: строка со словами заклинания — кастеру
                    // «вы», наблюдателям — имя; урон жирный (§8).
                    let hit = CombatHit {
                        source: char_id.clone(),
                        source_name: by_name.clone(),
                        target: target.to_string(),
                        target_name: mob_name.clone(),
                        kind: CombatKind::Rune,
                        amount: damage,
                        crit: false,
                        miss: false,
                        text: String::new(),
                        extra: Default::default(),
                    };
                    for addr in world.chars_in_location(&location) {
                        let mut per = hit.clone();
                        per.text = crate::combat_log::spell_hit_text(
                            &spell,
                            &by_name,
                            &mob_name,
                            damage,
                            addr == char_id,
                        );
                        plan.push((
                            addr.clone(),
                            event_payload_text(names::EVT_COMBAT_HIT, &per).unwrap_or_default(),
                        ));
                        plan.push((
                            addr,
                            event_payload_text(
                                names::EVT_ENTITY_HP,
                                &EntityHp {
                                    entity_id: target.to_string(),
                                    hp: Hp {
                                        cur: hp,
                                        max: mob_max,
                                    },
                                },
                            )
                            .unwrap_or_default(),
                        ));
                    }
                    if hp == 0 {
                        xp_work = crate::tick::mob_kill_effects(
                            &mut world, &mut plan, target, &location, &char_id, skills_cfg,
                        );
                        if let Some(work) = &mut xp_work {
                            // XP убийства — в школу заклинания (skills.md §2.3).
                            work.skill = school.clone();
                        }
                    }
                }
            }
            for (addr, text) in plan {
                if text.is_empty() {
                    continue;
                }
                if addr == char_id {
                    mine.push(Out::Text(text));
                } else {
                    others.push((addr, text));
                }
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
        {
            let world = self.state.world.lock().expect("мир");
            for (addr, text) in others {
                world.send_to_char(&addr, text);
            }
        }
        // M17/F30g: кулдаун каста → таймер кнопки.
        mine.push(self.item_cd_started(&char_id, &rune_def));
        mine
    }

    /// `cmd/magic.scroll_cast` (§7): свиток расходуется, мана не тратится;
    /// `scroll_requires_target` → цель обязательна (`E_TARGET_GONE`).
    pub(super) async fn magic_scroll_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MagicScrollCast>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(scroll) = (match db::items::find(&pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("scroll_cast (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "свитка нет")];
        };
        let Some(scroll_def) = (match db::items::def(&pool, &scroll.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("scroll_cast (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения свитка",
            )];
        };
        if scroll_def.item_type != "scroll" {
            return vec![Self::err(seq, ErrorCode::ItemGone, "это не свиток")];
        }
        // M17/F30g: кулдаун каста свитка (cooldown_s, дефолт 1 с).
        if let Some(err) = self.item_cd_gate(seq, &char_id, &scroll_def).await {
            return vec![err];
        }

        let location = self.current_location(&char_id).await;
        let requires_target = self.state.config.gameplay.magic.scroll_requires_target;
        if requires_target && cmd.target_id.is_none() {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не указана")];
        }
        if let Some(target) = cmd.target_id.as_deref() {
            let alive = {
                let world = self.state.world.lock().expect("мир");
                world.mob_location(target).as_deref() == Some(location.as_str())
            };
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        let spell = scroll_def
            .stats
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let damage = scroll_def
            .stats
            .get("damage")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let school = scroll_def
            .stats
            .get("school")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Расход свитка — до событий (§7: consumable).
        let mut consumable = Vec::new();
        if scroll.qty > 1 {
            let new_qty = i64::from(scroll.qty) - 1;
            if let Err(e) = db::items::set_qty(&pool, &scroll.uid, new_qty).await {
                warn!("scroll_cast (qty): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            consumable.push(out_event(
                names::EVT_ITEM_STATE,
                None,
                serde_json::to_value(ItemState {
                    uid: scroll.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        } else {
            if let Err(e) = db::items::delete(&pool, &scroll.uid).await {
                warn!("scroll_cast (удаление): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            consumable.push(out_event(
                names::EVT_ITEM_REMOVED,
                None,
                serde_json::to_value(ItemRemoved {
                    uid: scroll.uid.clone(),
                    reason: ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            ));
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
        let cast = MagicCast {
            by: char_id.clone(),
            by_name: by_name.clone(),
            spell: spell.clone(),
            what: CastSource::Scroll,
            target: cmd.target_id.clone(),
            charges_left: None,
            mp_left: None,
            ok: true,
            extra: Default::default(),
        };
        let cast_json = event_payload_text(names::EVT_MAGIC_CAST, &cast);

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(text) = &cast_json {
            mine.push(Out::Text(text.clone()));
        }
        mine.extend(consumable);

        // cast-событие — остальным в локации (свой уже в `mine`).
        if let Some(text) = &cast_json {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&location) {
                if addr != char_id {
                    world.send_to_char(&addr, text.clone());
                }
            }
        }

        let mut xp_work: Option<crate::tick::KillXp> = None;
        if let Some(target) = cmd.target_id.as_deref() {
            let skills_cfg = &self.state.config.skills;
            let mut plan: Vec<(String, String)> = Vec::new();
            {
                let mut world = self.state.world.lock().expect("мир");
                // M32: магическое сопротивление моба снижает урон.
                let damage = crate::systems::after_resist(damage, world.mob_resist(target));
                if let Some(hp) = world.damage_mob(target, damage) {
                    let mob_name = world
                        .mob(target)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    let mob_max = world.mob_max_hp(target);
                    // M17/F30f: свиток кастует заклинание — слова и
                    // «чем бьёт» как у руны (§8).
                    let hit = CombatHit {
                        source: char_id.clone(),
                        source_name: by_name.clone(),
                        target: target.to_string(),
                        target_name: mob_name.clone(),
                        kind: CombatKind::Scroll,
                        amount: damage,
                        crit: false,
                        miss: false,
                        text: String::new(),
                        extra: Default::default(),
                    };
                    for addr in world.chars_in_location(&location) {
                        let mut per = hit.clone();
                        per.text = crate::combat_log::spell_hit_text(
                            &spell,
                            &by_name,
                            &mob_name,
                            damage,
                            addr == char_id,
                        );
                        plan.push((
                            addr.clone(),
                            event_payload_text(names::EVT_COMBAT_HIT, &per).unwrap_or_default(),
                        ));
                        plan.push((
                            addr,
                            event_payload_text(
                                names::EVT_ENTITY_HP,
                                &EntityHp {
                                    entity_id: target.to_string(),
                                    hp: Hp {
                                        cur: hp,
                                        max: mob_max,
                                    },
                                },
                            )
                            .unwrap_or_default(),
                        ));
                    }
                    if hp == 0 {
                        xp_work = crate::tick::mob_kill_effects(
                            &mut world, &mut plan, target, &location, &char_id, skills_cfg,
                        );
                        if let Some(work) = &mut xp_work {
                            work.skill = school.clone();
                        }
                    }
                }
            }
            for (addr, text) in plan {
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
        // M17/F30g: кулдаун каста → таймер кнопки.
        mine.push(self.item_cd_started(&char_id, &scroll_def));
        mine
    }
}

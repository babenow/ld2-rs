//! RF1i: добыча (ФР-03/04, M19) — разделка трупов и сбор ресурсов,
//! прогресс добычи и прерывание по движению.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::ResourceGather;
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{CraftProfession, EntityDespawn, EntityMove, ItemAdded, ItemState};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::event_payload_text;
use super::rules::item_snapshot;
use super::{out_event, Out, Session};
use crate::db;

impl Session {
    /// Снятие шкур: старт разделки — сервер-джоб на5 с (тиктаймер
    /// шлёт `evt/harvest/result`); все проверки — до старта:
    /// труп в локации с флагом действия и не сделан, профессия
    /// изучена, нож в инвентаре, гейт уровней (треб − уровень <25),
    /// нет активного джоба на этом трупе. M24: старт на **другом**
    /// трупе разрешён — текущая разделка прерывается, шкура
    /// брошенного трупа портится (для gut и skin одинаково).
    pub(super) async fn corpse_harvest(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ld2_protocol::cmd::CorpseHarvest>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.action != "skin" && cmd.action != "gut" {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректное действие")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.current_location(&char_id).await;
        // Труп, флаги, «уже сделано» — в мире.
        let (name, required) = {
            let world = self.state.world.lock().expect("мир");
            // M24: переключение на ДРУГОЙ труп разрешено — текущая
            // разделка прерывается ниже (шкура брошенного трупа
            // портится); повторный старт на том же трупе — E_COOLDOWN.
            let prev = world.harvest_job(&char_id).map(|j| j.corpse_id.clone());
            if prev.as_deref() == Some(cmd.entity_id.as_str()) {
                return vec![Self::err(
                    seq,
                    ErrorCode::Cooldown,
                    "вы уже разделываете труп",
                )];
            }
            let Some(corpse) = world.corpse(&cmd.entity_id) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
            };
            if corpse.location_id != current {
                return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
            }
            let Some(harv) = corpse.harvest.as_ref() else {
                return vec![Self::err(
                    seq,
                    ErrorCode::TargetGone,
                    "это животное разделывать нельзя",
                )];
            };
            let allowed = harv
                .get(&cmd.action)
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !allowed {
                return vec![Self::err(
                    seq,
                    ErrorCode::TargetGone,
                    "это животное разделывать нельзя",
                )];
            }
            let done = if cmd.action == "skin" {
                corpse.skin_done
            } else {
                corpse.gut_done
            };
            if done {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже разделано")];
            }
            let required = harv
                .get("level")
                .and_then(|v| v.as_i64())
                .unwrap_or(1)
                .clamp(1, 100) as u32;
            let name = corpse
                .name
                .strip_prefix("Труп: ")
                .unwrap_or(&corpse.name)
                .to_string();
            (name, required)
        };
        // Профессия изучена → уровень (гейт: треб − уровень <25).
        let learned = match db::professions::learned(&self.state.pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("harvest (профессии): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some((_, _, _, level)) = learned.into_iter().find(|(id, _, _, _)| id == "skinning")
        else {
            return vec![Self::err(
                seq,
                ErrorCode::Profession,
                "профессия не изучена",
            )];
        };
        let level = level.max(0) as u32;
        if required.saturating_sub(level) >= 25 {
            return vec![Self::err(
                seq,
                ErrorCode::LevelRequired,
                format!(
                    "не хватает {} уровней (требуется {required})",
                    required.saturating_sub(level)
                ),
            )];
        }
        // Нож для снятия шкур (паритет инструмента крафта).
        let has_knife =
            match db::items::has_item_type(&self.state.pool, &char_id, "skinning_knife").await {
                Ok(v) => v,
                Err(e) => {
                    warn!("harvest (нож): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        if !has_knife {
            return vec![Self::err(
                seq,
                ErrorCode::ToolMissing,
                "нужен нож для снятия шкур",
            )];
        }
        // M24: переключение — текущая разделка прерывается, шкура
        // брошенного трупа испорчена (для gut и skin одинаково:
        // снять шкуру после нельзя, повторить gut можно). Итог и
        // обновление трупа шлём ДО ack: клиент гасит бар по
        // evt/harvest/result и только потом получает старт нового.
        let interrupted = {
            let mut world = self.state.world.lock().expect("мир");
            let old = world.cancel_harvest(&char_id);
            if let Some(job) = &old {
                world.ruin_skin(&job.corpse_id);
            }
            old
        };
        let mut outs: Vec<Out> = Vec::new();
        if let Some(job) = interrupted {
            let result = ld2_protocol::evt::HarvestResult {
                entity_id: job.corpse_id.clone(),
                name: job.name.clone(),
                action: job.action.clone(),
                ok: false,
                ruined: true,
            };
            if let Some(text) = event_payload_text("evt/harvest/result", &result) {
                outs.push(Out::Text(text));
            }
            // Флаги «шкура испорчена» — всем в локации (окно подятия).
            outs.extend(self.broadcast_corpse(&job.corpse_id));
        }
        // Старт джоба (5 с).
        let at = crate::time::now_epoch() + 5;
        {
            let mut world = self.state.world.lock().expect("мир");
            world.start_harvest(crate::world::HarvestJob {
                char_id: char_id.clone(),
                corpse_id: cmd.entity_id.clone(),
                action: cmd.action.clone(),
                name: name.clone(),
                at,
            });
        }
        outs.push(Self::ack_payload(
            seq,
            &serde_json::json!({ "action": cmd.action, "name": name }),
        ));
        outs
    }

    /// M20/F20b: добыча ресурса — инструмент (`item_type` в
    /// инвентаре), ранг профессии, выдача 1 шт. + XP (паритет крафта),
    /// уменьшение запаса; патч шкалы/десктронизация — всем в локации.
    pub(super) async fn resource_gather(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ResourceGather>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        // 1) узел: в текущей локации, видим, запас жив.
        let node = {
            let world = self.state.world.lock().expect("мир");
            match world.resource(&cmd.entity_id) {
                Some(n) if n.location_id == current && !n.hidden && n.current > 0 => n.clone(),
                _ => {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "ресурс недоступен")];
                }
            }
        };
        // 2) инструмент: предмет с item_type (паритет инструмента крафта).
        if let Some(tool) = &node.tool {
            let has = match db::items::has_item_type(pool, &char_id, tool).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("gather (инструмент): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if !has {
                return vec![Self::err(seq, ErrorCode::ToolMissing, "нужен инструмент")];
            }
        }
        // 3) ранг/уровень профессии (профессия должна быть изучена).
        let mut prof_xp: Option<(String, i64, i32, i32)> = None;
        if let Some(prof) = &node.profession {
            let learned = match db::professions::learned(pool, &char_id).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("gather (профессии): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            match learned.iter().find(|(id, _, _, _)| id == prof) {
                Some((_, xp, rank, level)) if *rank >= node.min_rank => {
                    prof_xp = Some((prof.clone(), *xp, *rank, *level));
                }
                Some(_) => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Profession,
                        "ранг профессии ниже требуемого",
                    )];
                }
                None => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Profession,
                        "профессия не изучена",
                    )];
                }
            }
        }
        // 4) место в инвентаре до уменьшения запаса (паритет take_loot).
        let slots = self.state.config.gameplay.inventory.slots;
        match Self::take_loot_fits(pool, &char_id, &node.def_id, slots).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::InventoryFull,
                    "инвентарь заполнен",
                )];
            }
            Err(e) => {
                warn!("gather (слоты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        // 5) добыча в мире: −1 запас (при 0 — респавн, трава — скрытие).
        let after = {
            let mut world = self.state.world.lock().expect("мир");
            world.try_gather(&cmd.entity_id, &current)
        };
        let Some(after) = after else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "ресурс исчерпан")];
        };
        // 6) выдача предмета (место проверено выше).
        let Some(def) = (match db::items::def(pool, &node.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("gather (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения")];
        };
        let (row, stacked) = match db::items::grant_stacking(pool, &char_id, &node.def_id, 1).await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("gather (выдача): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        let item = item_snapshot(&row, &def);
        outs.push(if stacked {
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(row.qty));
            out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: row.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            )
        } else {
            out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            )
        });
        // 7) XP профессии (паритет крафта) → снапшот себе.
        if let Some((prof, xp_old, rank, level)) = prof_xp {
            let cfg = &self.state.config.gameplay.professions;
            let gain = cfg.xp_per_craft_base + cfg.xp_per_craft_per_rank * i64::from(rank);
            // M21: опыт растит УРОВЕНЬ; ранг меняет только тренер/квест.
            let (xp_new, level_new) =
                crate::systems::profession_level_up(cfg, xp_old, level.max(0) as u32, gain);
            if let Err(e) = db::professions::grant_progress(
                pool,
                &char_id,
                &prof,
                xp_new,
                rank,
                level_new as i32,
            )
            .await
            {
                warn!("gather (XP профессии {char_id}): {e}");
            } else {
                let snapshot =
                    crate::systems::profession_snapshot(cfg, &prof, xp_new, rank, level_new);
                let name = db::professions::profession_name(pool, &prof)
                    .await
                    .ok()
                    .flatten();
                let text = event_payload_text(
                    "evt/craft/profession",
                    &CraftProfession {
                        profession: snapshot,
                        name,
                        gain: Some(gain),
                    },
                );
                if let Some(text) = text {
                    outs.push(Out::Text(text));
                }
            }
        }
        // 8) запас в локации: трава исчезла → despawn, жила/дерево →
        // патч шкалы (паттерн entity/move).
        let world_evt = if after.hidden {
            event_payload_text(
                "evt/entity/despawn",
                &EntityDespawn {
                    entity_id: after.id.clone(),
                    location_id: after.location_id.clone(),
                    kind: Some(ld2_protocol::types::EntityKind::Resource),
                    picked_by: None,
                    picked_gender: None,
                },
            )
        } else {
            let mut patch = serde_json::Map::new();
            patch.insert(
                "hp".into(),
                serde_json::json!({"cur": after.current, "max": after.max}),
            );
            event_payload_text(
                "evt/entity/move",
                &EntityMove {
                    entity_id: after.id.clone(),
                    patch,
                },
            )
        };
        {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&after.location_id) {
                if let Some(text) = &world_evt {
                    world.send_to_char(&addr, text.clone());
                }
            }
        }
        outs
    }
}

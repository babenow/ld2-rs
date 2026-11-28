//! RF1i: добыча (ФР-03/04, M19) — разделка трупов и сбор ресурсов,
//! прогресс добычи и прерывание по движению.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::names;

use ld2_protocol::cmd::ResourceGather;
use ld2_protocol::envelope::Envelope;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::event_payload_text;
use super::{Out, Session};
use crate::consts::{
    ACTION_GUT, ACTION_SKIN, ITEM_SKINNING_KNIFE, PROFESSION_MINING, PROFESSION_SKINNING,
};
use crate::db;

const MIN_LEVEL: i64 = 1;
const MAX_LEVEL: i64 = 100;
const LEVEL_GAP_MAX: u32 = 25;
const JOB_SECONDS: i64 = 5;

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
        if cmd.action != ACTION_SKIN && cmd.action != ACTION_GUT {
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
            let done = if cmd.action == ACTION_SKIN {
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
                .clamp(MIN_LEVEL, MAX_LEVEL) as u32;
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
        let Some((_, _, _, level)) = learned
            .into_iter()
            .find(|(id, _, _, _)| id == PROFESSION_SKINNING)
        else {
            return vec![Self::err(
                seq,
                ErrorCode::Profession,
                "профессия не изучена",
            )];
        };
        let level = level.max(0) as u32;
        if required.saturating_sub(level) >= LEVEL_GAP_MAX {
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
            match db::items::has_item_type(&self.state.pool, &char_id, ITEM_SKINNING_KNIFE).await {
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
            let result = job.result(false, true, None);
            if let Some(text) = event_payload_text(names::EVT_HARVEST_RESULT, &result) {
                outs.push(Out::Text(text));
            }
            // Флаги «шкура испорчена» — всем в локации (окно подятия).
            outs.extend(self.broadcast_corpse(&job.corpse_id));
        }
        // Старт джоба (5 с).
        let at = crate::time::now_epoch() + JOB_SECONDS;
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

    /// M20/F20b + M39: добыча ресурса — валидации (инструмент, ранг,
    /// уровень, место в инвентаре), затем сервер-джоб 5 с; выдача руды,
    /// камня, XP и обновление запаса — в `gather_step`.
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
                Some(n) if n.location_id == current && !n.hidden => {
                    if n.profession.as_deref() == Some(PROFESSION_MINING) {
                        return vec![Self::err(
                            seq,
                            ErrorCode::VeinEmpty,
                            "В жиле нет больше руды. Подождите, пока руда появится снова",
                        )];
                    }
                    return vec![Self::err(
                        seq,
                        ErrorCode::TargetGone,
                        "ресурс исчерпан — подождите возрождения",
                    )];
                }
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
        let mut gather_chance: Option<f64> = None;
        let mut gather_seconds = JOB_SECONDS;
        if let Some(prof) = &node.profession {
            let learned = match db::professions::learned(pool, &char_id).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("gather (профессии): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let Some((_, _, rank, level)) = learned.iter().find(|(id, _, _, _)| id == prof) else {
                return vec![Self::err(
                    seq,
                    ErrorCode::Profession,
                    "профессия не изучена",
                )];
            };
            if *rank < node.min_rank {
                return vec![Self::err(
                    seq,
                    ErrorCode::Profession,
                    "ранг профессии ниже требуемого",
                )];
            }
            // M38: уровень жилы, допуск 25 (как у снятия шкур).
            let level_u = (*level).max(0) as u32;
            if node.required_level.saturating_sub(level_u) >= 25 {
                return vec![Self::err(
                    seq,
                    ErrorCode::LevelRequired,
                    format!(
                        "не хватает {} уровней (требуется {})",
                        node.required_level.saturating_sub(level_u),
                        node.required_level
                    ),
                )];
            }
            if prof == PROFESSION_MINING {
                gather_chance = Some(crate::systems::action_chance(
                    &self.state.config.gameplay.professions,
                    node.required_level,
                    level_u,
                ));
                gather_seconds =
                    crate::systems::mining_gather_seconds(node.required_level, level_u);
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
        // 5) M39: сбор по времени — 2..6 с (руда) / 5 с, выдача в тике.
        let at = crate::time::now_ms() + gather_seconds * 1000;
        let (char_name, gender) = self
            .char
            .as_ref()
            .map(|c| (c.name.clone(), c.gender.clone()))
            .unwrap_or_default();
        {
            let mut world = self.state.world.lock().expect("мир");
            world.start_gather(crate::world::GatherJob {
                char_id: char_id.clone(),
                char_name,
                gender,
                entity_id: cmd.entity_id.clone(),
                name: node.name.clone(),
                chance: gather_chance,
                at,
            });
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }
}

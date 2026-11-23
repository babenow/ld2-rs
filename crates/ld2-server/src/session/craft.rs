//! RF1p: ремесло (M23) — профессии, рецепты, изучение, сброс, ранг и
//! запуск работы.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{CraftLearn, CraftRankUp, CraftRecipes, CraftReset, CraftStart};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::CraftProfession;
use ld2_protocol::types::{Entity, ProfessionSnapshot};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::rules::recipe_payload;
use super::{out_event, Out, Session};
use crate::content::ContentInput;
use crate::db;
use crate::trainers;

impl Session {
    /// Профессия персонажа → снапшот §6 (`xp_to_next` из конфига, D25).
    pub(super) fn profession_snapshot(
        &self,
        id: &str,
        xp: i64,
        rank: i32,
        level: u32,
    ) -> ProfessionSnapshot {
        // M21: общая сборка (уровень/ранг/extra) — в systems.
        crate::systems::profession_snapshot(
            &self.state.config.gameplay.professions,
            id,
            xp,
            rank,
            level,
        )
    }

    /// `cmd/craft.list` (§7): мои профессии `{professions[]}`.
    pub(super) async fn craft_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let rows = match db::professions::learned(&self.state.pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.list: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let professions: Vec<ProfessionSnapshot> = rows
            .iter()
            .map(|(id, xp, rank, level)| self.profession_snapshot(id, *xp, *rank, *level as u32))
            .collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "professions": professions }),
        )]
    }

    /// `cmd/craft.recipes` (§7): список рецептов `{recipes[]}` (payload
    /// skills.md §5.4; фильтр по профессии опционален).
    pub(super) async fn craft_recipes(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftRecipes>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let rows = match db::craft::recipes(&self.state.pool, cmd.profession.as_deref()).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.recipes: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let recipes: Vec<serde_json::Value> = rows.iter().map(recipe_payload).collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "recipes": recipes }),
        )]
    }

    /// `cmd/craft.learn` (§7): изучение профессии; лимит первичных —
    /// `profession_limit` (D25) → `E_PROFESSION_LIMIT`.
    pub(super) async fn craft_learn(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftLearn>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Локация до заимствования pool (current_location требует &mut self).
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;

        let prof_type = match sqlx::query_scalar::<_, String>(
            "SELECT prof_type FROM professions WHERE id = $1",
        )
        .bind(&cmd.profession_id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(t)) => t,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::Profession, "нет такой профессии")];
            }
            Err(e) => {
                warn!("craft.learn (профессия): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // Уже изучена → идемпотентный ack без повторной оплаты (§7, M14).
        // Профессии живут в character_professions, а не в character_skills.
        match db::professions::learned(pool, &char_id).await {
            Ok(rows) => {
                if let Some((_, xp, rank, level)) = rows
                    .into_iter()
                    .find(|(id, _, _, _)| id == &cmd.profession_id)
                {
                    let snapshot =
                        self.profession_snapshot(&cmd.profession_id, xp, rank, level as u32);
                    return vec![Self::ack_payload(
                        seq,
                        &serde_json::json!({ "profession": snapshot }),
                    )];
                }
            }
            Err(e) => {
                warn!("craft.learn (изученные): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }

        // M14: обучение только у тренера в локации (E_NO_TRAINER).
        let Some(price) = (match trainers::trainer_price(
            pool,
            &location,
            trainers::TrainerKind::Profession,
            &cmd.profession_id,
        )
        .await
        {
            Ok(price) => price,
            Err(e) => {
                warn!("craft.learn (тренер): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::NoTrainer,
                "здесь нет тренера по этой профессии",
            )];
        };

        if prof_type == "primary" {
            let limit = i64::from(self.state.config.gameplay.profession_limit);
            match db::professions::primary_count(pool, &char_id).await {
                Ok(count) if count >= limit => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::ProfessionLimit,
                        format!("лимит первичных профессий: {limit}"),
                    )];
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("craft.learn (счётчик): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }

        // M14: оплата тренеру (новая профессия — ранг 0 → базовая цена).
        let gold = trainers::gold_price(&price, 0);
        // F29a: оплата монетами — события списания в конце ответа.
        let money_events = match self.pay_trainer(seq, &char_id, &price, gold).await {
            Ok(events) => events,
            Err(e) => return vec![e],
        };

        match db::professions::learn(pool, &char_id, &cmd.profession_id).await {
            Ok(_) => {}
            Err(e) => {
                warn!("craft.learn: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let snapshot = self.profession_snapshot(&cmd.profession_id, 0, 1, 0);
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "profession": snapshot }),
        )];
        outs.push(out_event(
            "evt/craft/profession",
            None,
            serde_json::to_value(CraftProfession {
                profession: snapshot,
                name: None,
                gain: None,
            })
            .unwrap_or_default(),
        ));
        outs.extend(money_events);
        outs
    }

    /// Окно профессий: сброс изученной профессии — прогресс теряется,
    /// слот основной освобождается; ack несёт `profession_id`
    /// (`E_PROFESSION` — не изучена).
    pub(super) async fn craft_reset(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftReset>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        match db::professions::forget(&self.state.pool, &char_id, &cmd.profession_id).await {
            Ok(true) => vec![Self::ack_payload(
                seq,
                &serde_json::json!({ "profession_id": cmd.profession_id }),
            )],
            Ok(false) => {
                vec![Self::err(
                    seq,
                    ErrorCode::Profession,
                    "профессия не изучена",
                )]
            }
            Err(e) => {
                warn!("craft.reset ({}): {e}", cmd.profession_id);
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// M21: повышение ранга **у тренера** (`cmd/craft.rank_up`) —
    /// уровень набран до границы (`E_LEVEL_REQUIRED`), высший ранг →
    /// `E_RANK_CAP`, золото по прогрессии `200 × 1.5^ранг`
    /// (`E_INGREDIENTS`); ack несёт `{profession}` + событие.
    pub(super) async fn craft_rank_up(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftRankUp>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        // Тренер по этой профессии в локации (E_NO_TRAINER).
        let trainer = match trainers::trainer_price(
            pool,
            &location,
            trainers::TrainerKind::Profession,
            &cmd.profession_id,
        )
        .await
        {
            Ok(price) => price,
            Err(e) => {
                warn!("rank_up (тренер): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if trainer.is_none() {
            return vec![Self::err(
                seq,
                ErrorCode::NoTrainer,
                "здесь нет тренера по этой профессии",
            )];
        }
        // Изучена → (xp, rank, level).
        let learned = match db::professions::learned(pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("rank_up (профессии): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some((_, xp, rank, level)) = learned
            .into_iter()
            .find(|(id, _, _, _)| id == &cmd.profession_id)
        else {
            return vec![Self::err(
                seq,
                ErrorCode::Profession,
                "профессия не изучена",
            )];
        };
        let cfg = &self.state.config.gameplay.professions;
        let rank_u32 = rank.max(1) as u32;
        if rank_u32 >= crate::systems::profession_rank_max(cfg) {
            return vec![Self::err(seq, ErrorCode::RankCap, "достигнут высший ранг")];
        }
        // Граница следующего ранга: level ≥ rank × span.
        let need_level = rank_u32.saturating_mul(cfg.rank_span);
        if (level.max(0) as u32) < need_level {
            return vec![Self::err(
                seq,
                ErrorCode::LevelRequired,
                format!("нужен уровень {need_level} для ранга {}", rank_u32 + 1),
            )];
        }
        // Золото по прогрессии (200 × 1.5^ранг) — та же касса, что и
        // обучение (монеты-стаки, F29a).
        let cost = crate::systems::profession_rank_up_cost(cfg, rank_u32);
        let price = trainers::TrainerPrice {
            gold_base: cost,
            gold_per_rank: 0,
            items: Vec::new(),
        };
        let money_events = match self.pay_trainer(seq, &char_id, &price, cost).await {
            Ok(events) => events,
            Err(e) => return vec![e],
        };
        let new_rank = rank + 1;
        if let Err(e) =
            db::professions::grant_progress(pool, &char_id, &cmd.profession_id, xp, new_rank, level)
                .await
        {
            warn!("rank_up ({}): {e}", cmd.profession_id);
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let snapshot =
            self.profession_snapshot(&cmd.profession_id, xp, new_rank, level.max(0) as u32);
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "profession": snapshot }),
        )];
        outs.push(out_event(
            "evt/craft/profession",
            None,
            serde_json::to_value(CraftProfession {
                profession: snapshot,
                name: None,
                gain: None,
            })
            .unwrap_or_default(),
        ));
        outs.extend(money_events);
        outs
    }

    /// `cmd/craft.start` (§7): проверки (ранг/станок/инструмент/ингредиенты)
    /// → списание ингредиентов + джоб в БД (D27); итог — `evt/craft/result`
    /// по готовности (тик-луп). Ошибки: `E_RANK_CAP`, `E_STATION_REQUIRED`,
    /// `E_TOOL_MISSING`, `E_INGREDIENTS`.
    pub(super) async fn craft_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftStart>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.qty < 1 || cmd.qty > 100 {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "qty должно быть 1..=100",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(recipe) = (match db::craft::recipe(&pool, &cmd.recipe_id).await {
            Ok(r) => r,
            Err(e) => {
                warn!("craft.start (рецепт): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "рецепта нет")];
        };

        // Ранг профессии.
        let learned = match db::professions::learned(&pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.start (профессии): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let prof_rank = learned
            .iter()
            .find(|(id, _, _, _)| *id == recipe.profession)
            .map(|(_, _, rank, _)| *rank)
            .unwrap_or(0);
        if prof_rank < recipe.rank {
            return vec![Self::err(
                seq,
                ErrorCode::RankCap,
                format!("нужен ранг {} профессии {}", recipe.rank, recipe.profession),
            )];
        }

        // Станок: сущность с `extra.station` в текущей локации.
        if let Some(station) = recipe.station.as_deref() {
            let location = self.current_location(&char_id).await;
            let has_station = match db::locations::get(&pool, &location).await {
                Ok(Some(row)) => {
                    let entities: Vec<Entity> =
                        serde_json::from_value(row.entities.clone()).unwrap_or_default();
                    entities
                        .iter()
                        .any(|e| e.extra.get("station").and_then(|s| s.as_str()) == Some(station))
                }
                _ => false,
            };
            if !has_station {
                return vec![Self::err(
                    seq,
                    ErrorCode::StationRequired,
                    format!("нужен станок: {station}"),
                )];
            }
        }

        // Инструмент: предмет с `item_type` в инвентаре.
        if let Some(tool) = recipe.tool_type.as_deref() {
            let has_tool = match db::items::inventory_has_type(&pool, &char_id, tool).await {
                Ok(has) => has,
                Err(e) => {
                    warn!("craft.start (инструмент): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if !has_tool {
                return vec![Self::err(
                    seq,
                    ErrorCode::ToolMissing,
                    format!("нет инструмента: {tool}"),
                )];
            }
        }

        // Ингредиенты: qty × inputs.
        let inputs: Vec<ContentInput> =
            serde_json::from_value(recipe.inputs.clone()).unwrap_or_default();
        for input in &inputs {
            let need = input.qty * cmd.qty;
            let have = match db::items::count_by_type(&pool, &char_id, &input.item_type).await {
                Ok(have) => have,
                Err(e) => {
                    warn!("craft.start (ингредиенты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if have < need {
                return vec![Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("не хватает {}: {have}/{need}", input.item_type),
                )];
            }
        }
        for input in &inputs {
            let need = input.qty * cmd.qty;
            if let Err(e) =
                db::items::consume_by_type(&pool, &char_id, &input.item_type, need).await
            {
                warn!("craft.start (списание): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }

        // Джоб: материалы уже списаны, рестарт не теряет их (D27).
        let finishes_at =
            crate::time::now_epoch() as f64 + f64::from(recipe.time_s) * cmd.qty as f64;
        if let Err(e) = db::craft::enqueue(&pool, &char_id, &recipe, cmd.qty, finishes_at).await {
            warn!("craft.start (джоб): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }
}

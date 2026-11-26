//! RF1o: квесты (M21) — список, взятие/сдача/отказ и переходы статусов.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::CraftProfession;
use ld2_protocol::types::ProfessionSnapshot;
use ld2_protocol::ErrorCode;
use serde_json::Value;
use tracing::warn;

use super::{out_event, Out, Session};
use crate::db;

impl Session {
    /// Журнал квестов: все контент-квесты + прогресс персонажа (§7).
    pub(super) async fn quest_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let (defs, progress) = match (
            db::quests::defs(&self.state.pool).await,
            db::quests::progress(&self.state.pool, &char_id).await,
        ) {
            (Ok(d), Ok(p)) => (d, p),
            (Err(e), _) | (_, Err(e)) => {
                warn!("quest.list: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let quests: Vec<serde_json::Value> = defs
            .iter()
            .map(|def| {
                let row = progress.iter().find(|(id, _, _)| id == &def.0);
                serde_json::to_value(db::quests::build_state(def, row)).unwrap_or_default()
            })
            .collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "quests": quests }),
        )]
    }

    /// `cmd/quest.accept` (§7): взять квест → ack `{quest}` +
    /// `evt/quest/updated`.
    pub(super) async fn quest_accept(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "accept").await
    }

    /// `cmd/quest.turn_in` (§7): сдать выполненный → награда (XP в пул +
    /// предметы) → ack `{quest}` + события.
    pub(super) async fn quest_turn_in(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "turn_in").await
    }

    /// `cmd/quest.abandon` (§7): бросить → снова «доступен».
    pub(super) async fn quest_abandon(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "abandon").await
    }

    /// Общий переход квеста (accept/turn_in/abandon) — одна точка проверок.
    pub(super) async fn quest_transition(
        &mut self,
        env: &Envelope,
        seq: u32,
        op: &str,
    ) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(value) = Self::payload_of::<Value>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(quest_id) = value.get("quest_id").and_then(|q| q.as_str()) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет quest_id")];
        };
        let quest_id = quest_id.to_string();
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;

        let defs = match db::quests::defs(pool).await {
            Ok(defs) => defs,
            Err(e) => {
                warn!("quest (квесты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(def) = defs.iter().find(|d| d.0 == quest_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "квеста нет")];
        };
        let progress = match db::quests::progress(pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("quest (прогресс): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let row = progress.iter().find(|(id, _, _)| *id == quest_id);

        let mut outs = Vec::new();
        // M21: событие награды-ранга — отправляется ПОСЛЕ ack (иначе
        // клиент, ждущий ack, поглощает событие раньше времени).
        let mut rank_up_evt: Option<ProfessionSnapshot> = None;
        match op {
            "accept" => {
                // M18/F36: лимит текущих квестов — из конфига сервера
                // (§5 `quests.max_active`); взятые = active/turn_in.
                let current = progress
                    .iter()
                    .filter(|(_, state, _)| matches!(state.as_str(), "active" | "turn_in"))
                    .count();
                if current
                    >= usize::try_from(self.state.config.gameplay.quests.max_active)
                        .unwrap_or(usize::MAX)
                {
                    return vec![Self::err(
                        seq,
                        ErrorCode::QuestLimit,
                        "лимит текущих квестов достигнут",
                    )];
                }
                if row.is_some() {
                    return vec![Self::err(seq, ErrorCode::Internal, "квест уже взят")];
                }
                let goal_count = def.3.as_array().map(|a| a.len()).unwrap_or(0);
                if let Err(e) = db::quests::accept(pool, &char_id, &quest_id, goal_count).await {
                    warn!("quest (accept): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            "turn_in" => {
                let Some((_, state, _)) = row else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "квест не взят")];
                };
                if state != "turn_in" {
                    return vec![Self::err(seq, ErrorCode::Internal, "цели ещё не выполнены")];
                }
                // M21: награда «повышение ранга» — та же граница
                // уровней (ранг — без золота и тренера); проверка ДО
                // выдачи: квест остаётся «к сдаче» до набора уровня.
                let reward_rank_up = def
                    .4
                    .get("rank_up")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(prof_id) = reward_rank_up {
                    let learned = match db::professions::learned(pool, &char_id).await {
                        Ok(rows) => rows,
                        Err(e) => {
                            warn!("quest (rank_up профессии): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    };
                    match learned.into_iter().find(|(id, _, _, _)| *id == prof_id) {
                        None => {
                            return vec![Self::err(
                                seq,
                                ErrorCode::Profession,
                                "награда: профессия не изучена",
                            )];
                        }
                        Some((_, xp, rank, level)) => {
                            let cfg = &self.state.config.gameplay.professions;
                            let rank_u32 = rank.max(1) as u32;
                            if rank_u32 >= crate::systems::profession_rank_max(cfg) {
                                // Высший ранг достигнут — награда
                                // деградирует, квест сдаётся (без
                                // изменения pending).
                            } else {
                                let need_level = rank_u32.saturating_mul(cfg.rank_span);
                                if (level.max(0) as u32) < need_level {
                                    return vec![Self::err(
                                        seq,
                                        ErrorCode::LevelRequired,
                                        format!(
                                            "награда: нужен уровень {need_level} для ранга {}",
                                            rank_u32 + 1
                                        ),
                                    )];
                                }
                                // Применяем после set_state (ниже).
                                self.pending_rank_up = Some((prof_id, xp, rank + 1, level));
                            }
                        }
                    }
                }
                // Награда: XP в пул (D3/D4) + предметы (слоты — из конфига).
                let reward_xp = def.4.get("xp").and_then(|x| x.as_i64()).unwrap_or(0);
                let reward_items: Vec<(String, i64)> = def
                    .4
                    .get("items")
                    .and_then(|i| i.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|it| {
                                Some((
                                    it.get("type")?.as_str()?.to_string(),
                                    it.get("qty").and_then(|q| q.as_i64()).unwrap_or(1),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // R6: награда и завершение — одной транзакцией; обрыв
                // после выдачи награды больше не оставляет квест
                // «к сдаче» (повторная сдача = дюп XP/предметов).
                {
                    let mut tx = match pool.begin().await {
                        Ok(tx) => tx,
                        Err(e) => {
                            warn!("quest (tx): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    };
                    if reward_xp > 0 {
                        if let Err(e) =
                            db::characters::add_xp_pool(&mut *tx, &char_id, reward_xp).await
                        {
                            warn!("quest (награда XP): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    }
                    for (item_type, qty) in &reward_items {
                        if let Err(e) =
                            db::items::insert_by_type_tx(&mut tx, &char_id, item_type, *qty).await
                        {
                            warn!("quest (награда предмет): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    }
                    if let Err(e) =
                        db::quests::set_state(&mut *tx, &char_id, &quest_id, "done").await
                    {
                        warn!("quest (done): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    // M21: применить награду-ранг (граница проверена выше).
                    if let Some((prof_id, xp, new_rank, level)) = self.pending_rank_up.take() {
                        if let Err(e) = db::professions::grant_progress(
                            &mut *tx, &char_id, &prof_id, xp, new_rank, level,
                        )
                        .await
                        {
                            warn!("quest (rank_up {prof_id}): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                        rank_up_evt = Some(self.profession_snapshot(
                            &prof_id,
                            xp,
                            new_rank,
                            level.max(0) as u32,
                        ));
                    }
                    if let Err(e) = tx.commit().await {
                        warn!("quest (commit): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
            }
            _ => {
                // abandon: активный квест сбрасывается; завершённый — нельзя.
                if let Some((_, state, _)) = row {
                    if state == "done" {
                        return vec![Self::err(seq, ErrorCode::Internal, "квест завершён")];
                    }
                }
                if let Err(e) = db::quests::abandon(pool, &char_id, &quest_id).await {
                    warn!("quest (abandon): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }

        // Свежее состояние после перехода → ack `{quest}` + событие.
        let progress = db::quests::progress(pool, &char_id)
            .await
            .unwrap_or_default();
        let row = progress.iter().find(|(id, _, _)| *id == quest_id);
        let state = db::quests::build_state(def, row);
        outs.push(Self::ack_payload(
            seq,
            &serde_json::json!({ "quest": state }),
        ));
        outs.push(out_event(
            "evt/quest/updated",
            None,
            serde_json::json!({ "quest": state }),
        ));
        // Сдача: обновить пул в снапшоте сессии.
        if op == "turn_in" {
            if let Ok(Some(row)) = db::characters::get(pool, &char_id).await {
                if let Some(snap) = self.char.as_mut() {
                    snap.xp_pool = row.xp_pool;
                }
            }
        }
        // M21: награда-ранг — событие после ack (см. объявление выше).
        if let Some(profession) = rank_up_evt {
            outs.push(out_event(
                "evt/craft/profession",
                None,
                serde_json::to_value(CraftProfession {
                    profession,
                    name: None,
                    gain: None,
                })
                .unwrap_or_default(),
            ));
        }
        outs
    }
}

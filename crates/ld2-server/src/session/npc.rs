//! RF1n: NPC и диалоги (M20) — выбор ветки, отправка значений, тренеры,
//! квесты NPC и разговор/ответ.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{NpcReply, NpcTalk};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::types::Entity;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::{Out, Session};
use crate::db;

impl Session {
    /// NPC из сида текущей локации (паритет с `entity.info`);
    /// не-NPC/несущность → `E_TARGET_GONE`.
    pub(super) async fn require_npc(&self, seq: u32, entity_id: &str) -> Result<Entity, Out> {
        let pool = &self.state.pool;
        let current = self.location_id.clone().unwrap_or_default();
        let entities = match db::locations::get(pool, &current).await {
            Ok(Some(row)) => {
                serde_json::from_value::<Vec<Entity>>(row.entities.clone()).unwrap_or_default()
            }
            Ok(None) => Vec::new(),
            Err(e) => {
                warn!("npc (чтение {current}): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        };
        let Some(entity) = entities.into_iter().find(|e| e.id == entity_id) else {
            return Err(Self::err(seq, ErrorCode::TargetGone, "сущность не найдена"));
        };
        if entity.kind != ld2_protocol::types::EntityKind::Npc {
            return Err(Self::err(seq, ErrorCode::TargetGone, "это не NPC"));
        }
        Ok(entity)
    }

    /// M20 (заготовка): выбор действия в многостраничном диалоге —
    /// подтверждение без переходов (серверные переходы появятся с
    /// контентом игровых диалогов); ack с echo выбора.
    pub(super) async fn dialog_choice(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ld2_protocol::cmd::DialogChoice>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        tracing::debug!(
            "dialog.choice: dialog={} page={} action={}",
            cmd.dialog_id,
            cmd.page_id,
            cmd.action_id
        );
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "dialog_id": cmd.dialog_id,
                "page_id": cmd.page_id,
                "action_id": cmd.action_id,
            }),
        )]
    }

    /// M20 (заготовка): ввод execute-диалога — сервер принимает набор
    /// значений и возвращает его в ack (обратная связь «ввод получен»);
    /// игровая обработка (квесты/настройки) подключится позже.
    pub(super) async fn dialog_submit(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ld2_protocol::cmd::DialogSubmit>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        // M24: админ-диалоги количества (выдача/удаление) — своя ветка;
        // прочие диалоги — прежний echo (заготовка M20).
        if let Some(rest) = cmd.dialog_id.strip_prefix("admin.") {
            return self.admin_dialog_submit(rest, &cmd.values, seq).await;
        }
        tracing::debug!(
            "dialog.submit: dialog={} values={:?}",
            cmd.dialog_id,
            cmd.values
        );
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "dialog_id": cmd.dialog_id,
                "values": cmd.values,
            }),
        )]
    }

    /// Контекст персонажа для условий диалога (имя/пол/уровень/attrs/
    /// навыки/профессии/рюкзак — M18/F35).
    pub(super) async fn dialog_context(
        &self,
        char_id: &str,
        seq: u32,
    ) -> Result<crate::dialogs::DialogContext, Out> {
        let pool = &self.state.pool;
        let row = match db::characters::get(pool, char_id).await {
            Ok(Some(row)) => row,
            Ok(None) => return Err(Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")),
            Err(e) => {
                warn!("npc (лист): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        };
        let (level, _, _) =
            crate::systems::level_for_total(row.total_xp, &self.state.config.gameplay.level);
        let attrs: std::collections::BTreeMap<String, i64> =
            serde_json::from_value(row.attrs.clone()).unwrap_or_default();
        let mut skills = std::collections::HashMap::new();
        let mut professions = std::collections::HashMap::new();
        let mut items = std::collections::HashMap::new();
        match db::skills::learned(pool, char_id).await {
            Ok(rows) => {
                for s in rows {
                    skills.insert(s.skill_id, i64::from(s.rank));
                }
            }
            Err(e) => {
                warn!("npc (навыки): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        }
        match db::professions::learned(pool, char_id).await {
            Ok(rows) => {
                for p in rows {
                    professions.insert(p.0, i64::from(p.2));
                }
            }
            Err(e) => {
                warn!("npc (профессии): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        }
        match db::items::inventory(pool, char_id).await {
            Ok(rows) => {
                for it in rows {
                    *items.entry(it.def_id).or_insert(0) += i64::from(it.qty);
                }
            }
            Err(e) => {
                warn!("npc (рюкзак): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        }
        Ok(crate::dialogs::DialogContext {
            name: row.name,
            gender: row.gender,
            level: i64::from(level),
            attrs,
            skills,
            professions,
            items,
        })
    }

    /// Доступные квесты NPC (giver == id, фаза available): id-ы для
    /// гейта строк + wire-список для ack + флаг «NPC вообще даёт
    /// квесты» (кнопка статус-бара, M18/F38).
    pub(super) async fn npc_quests(
        &self,
        char_id: &str,
        npc_id: &str,
        seq: u32,
    ) -> Result<
        (
            std::collections::HashSet<String>,
            Vec<serde_json::Value>,
            bool,
        ),
        Out,
    > {
        let pool = &self.state.pool;
        let (defs, progress) = match (
            db::quests::defs(pool).await,
            db::quests::progress(pool, char_id).await,
        ) {
            (Ok(d), Ok(p)) => (d, p),
            (Err(e), _) | (_, Err(e)) => {
                warn!("npc (квесты): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        };
        let mut quests = Vec::new();
        let mut available = std::collections::HashSet::new();
        let mut gives_quests = false;
        for def in &defs {
            if def.2.as_deref() != Some(npc_id) {
                continue;
            }
            gives_quests = true;
            let progress_row = progress.iter().find(|(id, _, _)| id == &def.0);
            let state = db::quests::build_state(def, progress_row);
            if state.state == ld2_protocol::types::QuestPhase::Available {
                available.insert(state.id.clone());
                quests.push(serde_json::to_value(&state).unwrap_or_default());
            }
        }
        Ok((available, quests, gives_quests))
    }

    /// `cmd/npc.talk` (M18/F35/F37): NPC текущей локации → **корень
    /// ветки** (узел с видимыми ответами) и доступные квесты.
    /// ack `{dialog: {id, name}, node: {...}|null, quests[], actions[]}`;
    /// `node: null` — записи диалога нет (квесты отдаются). Поле
    /// `greeting` убрано (M18/F42, ADR-008).
    pub(super) async fn npc_talk(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<NpcTalk>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let entity = match self.require_npc(seq, &cmd.entity_id).await {
            Ok(entity) => entity,
            Err(out) => return vec![out],
        };
        let context = match self.dialog_context(&char_id, seq).await {
            Ok(context) => context,
            Err(out) => return vec![out],
        };
        let (available, quests, gives_quests) =
            match self.npc_quests(&char_id, &entity.id, seq).await {
                Ok(triple) => triple,
                Err(out) => return vec![out],
            };
        // M18/F38: контекстные кнопки статус-бара — только реально
        // существующие системы: «Квесты» (NPC — выдающий) и «Обучение»
        // (trainer в extra сущности); торговля/склад — когда появятся
        // их системы.
        let mut actions: Vec<&str> = Vec::new();
        if gives_quests {
            actions.push("quests");
        }
        if entity.extra.contains_key("trainer") {
            actions.push("train");
        }
        if self.state.content().shops.contains_key(&entity.id) {
            actions.push("shop");
        }
        let pool = &self.state.pool;
        let shown = match db::dialogs::shown_lines(pool, &char_id, &entity.id).await {
            Ok(map) => map,
            Err(e) => {
                warn!("npc.talk (кулдауны строк): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let now = crate::time::now_epoch();
        let ctx = context.line_ctx(&available);
        let content = self.state.content();
        let node = match content.dialogs.get(&entity.id) {
            Some(def) => {
                let root = def
                    .root
                    .clone()
                    .or_else(|| def.lines.first().map(|l| l.id.clone()))
                    .unwrap_or_default();
                // M18/F42: приветствие убрано (ADR-008) — окно открывается
                // сразу с корневой репликой.
                crate::dialogs::enter_node(def, &root, &ctx, now, &shown)
            }
            None => None,
        };
        if let Some((node, mark)) = &node {
            if *mark {
                if let Err(e) =
                    db::dialogs::mark_shown(pool, &char_id, &entity.id, &node.id, now).await
                {
                    // Показ уже уходит клиенту — кулдаун догонит в следующий раз.
                    warn!("npc.talk (отметка показа {}): {e}", node.id);
                }
            }
        }
        let node =
            serde_json::to_value(node.map(|(node, _)| node)).unwrap_or(serde_json::Value::Null);
        // M18/F44: обучение NPC из контента диалога (ADR-009) — список
        // навыков с ценой; у NPC без training — пустой массив.
        let training = self
            .state
            .content()
            .dialogs
            .get(&entity.id)
            .map(|def| {
                serde_json::to_value(&def.training).unwrap_or_else(|_| serde_json::json!([]))
            })
            .unwrap_or_else(|| serde_json::json!([]));
        // M21: профессии, которым обучает этот NPC (маркер trainer
        // сущности), с ценой `price_profession` — панель «Обучение»
        // учит и профессиям (изучение — cmd/craft.learn).
        let professions = {
            let marker = entity.extra.get("trainer");
            let ids: Vec<&str> = marker
                .and_then(|m| m.get("professions"))
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let price = marker
                .and_then(|m| m.get("price_profession"))
                .or_else(|| marker.and_then(|m| m.get("price")));
            let gold = price
                .and_then(|p| p.get("gold_base"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            ids.into_iter()
                .map(|id| serde_json::json!({ "id": id, "gold": gold }))
                .collect::<Vec<_>>()
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "dialog": {
                    "id": entity.id,
                    "name": entity.name,
                },
                "node": node,
                "quests": quests,
                "actions": actions,
                "training": training,
                "professions": professions,
            }),
        )]
    }

    /// `cmd/npc.reply` (M18/F37): ответ персонажа — переход по связи
    /// `node_id` → `reply_id`; у ответа без `next` диалог заканчивается
    /// (`{node: null}`). ack `{node: {...}|null, quests[]}` — список
    /// квестов освежается (приняли/сдали между открытиями).
    pub(super) async fn npc_reply(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<NpcReply>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let entity = match self.require_npc(seq, &cmd.entity_id).await {
            Ok(entity) => entity,
            Err(out) => return vec![out],
        };
        let content = self.state.content();
        let Some(def) = content.dialogs.get(&entity.id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "диалога нет")];
        };
        let context = match self.dialog_context(&char_id, seq).await {
            Ok(context) => context,
            Err(out) => return vec![out],
        };
        let (available, quests, _) = match self.npc_quests(&char_id, &entity.id, seq).await {
            Ok(triple) => triple,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let shown = match db::dialogs::shown_lines(pool, &char_id, &entity.id).await {
            Ok(map) => map,
            Err(e) => {
                warn!("npc.reply (кулдауны строк): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let now = crate::time::now_epoch();
        let ctx = context.line_ctx(&available);

        // Валидация связи (stateless): строка → ответ → цель.
        let Some(node_line) = def.lines.iter().find(|line| line.id == cmd.node_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет такой строки")];
        };
        let Some(reply) = node_line
            .replies
            .iter()
            .find(|reply| reply.id == cmd.reply_id)
        else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет такой реплики")];
        };
        if !crate::dialogs::reply_allowed(reply, &ctx) {
            return vec![Self::err(seq, ErrorCode::TargetGone, "реплика недоступна")];
        }
        let node = match &reply.next {
            // Конец диалога — без следующего узла.
            None => serde_json::Value::Null,
            Some(next_id) => {
                let Some((view, mark)) =
                    crate::dialogs::enter_node(def, next_id, &ctx, now, &shown)
                else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "строка недоступна")];
                };
                if mark {
                    if let Err(e) =
                        db::dialogs::mark_shown(pool, &char_id, &entity.id, next_id, now).await
                    {
                        warn!("npc.reply (отметка показа {next_id}): {e}");
                    }
                }
                serde_json::to_value(&view).unwrap_or_default()
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "node": node, "quests": quests }),
        )]
    }
}

//! RF1u: чат (M17) — отправка, удаление, доставка и история.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{ChatDelete, ChatSend};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::event_payload_text;
use super::{Out, Session};
use crate::db;

impl Session {
    /// `cmd/chat.send` (§7): dev-сценарии `/scenario …` (ФР-06/тесты);
    /// полноценный чат (ФР-13) — фаза M12.7.
    /// M14/F19: `cmd/chat.delete` — удаление сообщения из истории
    /// (только admin/moderator, §9 E_PERMISSION) + `evt/chat/deleted`
    /// всем онлайн (история общая — как system/global).
    pub(super) async fn chat_delete(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ChatDelete>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.is_moderator() {
            return vec![Self::err(
                seq,
                ErrorCode::Permission,
                "удалять сообщения могут только админ и модератор",
            )];
        }
        match db::chat::delete(&self.state.pool, cmd.id as i64).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::TargetGone,
                    "сообщение не найдено",
                )];
            }
            Err(e) => {
                warn!("chat (удаление): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let payload = ld2_protocol::evt::ChatDeleted { id: cmd.id };
        if let Some(text) = event_payload_text("evt/chat/deleted", &payload) {
            let world = self.state.world.lock().expect("мир");
            for addr in world.online_chars() {
                world.send_to_char(&addr, text.clone());
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    pub(super) async fn chat_send(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ChatSend>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if let Some(scenario) = cmd.text.strip_prefix("/scenario ") {
            // M24: дыра из бэклога — раньше префикс не проверялся
            // вовсе (любой игрок мог `/scenario give`); теперь —
            // админ либо явный флаг конфига `server.allow_scenario`
            // (dev/тесты); прод-конфиг без ключа → запрещено.
            if self.role != "admin" && !self.state.config.server.allow_scenario {
                return vec![Self::err(seq, ErrorCode::Permission, "сценарии отключены")];
            }
            return self.scenario(scenario.trim(), seq).await;
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Копия нужного из конфига (D25): cfg-заимствование мешает &mut ниже.
        let (channels, max_len, cooldown_s, history_limit) = {
            let chat = &self.state.config.gameplay.chat;
            (
                chat.channels.clone(),
                chat.max_len,
                chat.cooldown_s,
                chat.server_history,
            )
        };

        // Канал из конфига §5 (открытый список).
        if !channels.contains(&cmd.channel) {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                format!("нет канала: {}", cmd.channel),
            )];
        }
        // M14/F19: системный чат — только admin/moderator (§9 E_PERMISSION).
        if cmd.channel == "system" && !self.is_moderator() {
            return vec![Self::err(
                seq,
                ErrorCode::Permission,
                "недостаточно прав для system-канала",
            )];
        }
        // Длина текста (§12.2: chat.max_len → E_RATE_LIMITED).
        if cmd.text.chars().count() > max_len as usize {
            return vec![Self::err(
                seq,
                ErrorCode::RateLimited,
                format!("слишком длинное сообщение (до {max_len})"),
            )];
        }
        if cmd.text.trim().is_empty() {
            return vec![Self::err(seq, ErrorCode::Internal, "пустое сообщение")];
        }
        // Кулдаун (§12.2: chat.cooldown_s → E_RATE_LIMITED + retry_after_s).
        if let Some(last) = self.last_chat_at {
            let elapsed = last.elapsed().as_secs_f64();
            if elapsed < cooldown_s {
                let retry = (cooldown_s - elapsed).ceil().max(1.0) as u64;
                return vec![Self::ack_rate_limited(seq, retry)];
            }
        }

        // Рассылка: location — в локации, global/system — всем онлайн,
        // whisper — адресату (по имени) и автору, party/guild — участникам
        // (группы/гильдии — M12.7b/d: до тех пор только автор).
        let name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();
        let location = self.current_location(&char_id).await;
        // Каналы группы/гильдии (ФР-13): участникам своего состава.
        if cmd.channel == "party" {
            let members = {
                let social = self.state.social.lock().expect("соц");
                social.party_members(&char_id)
            };
            if members.is_empty() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "вы не в группе")];
            }
            return self
                .chat_deliver(seq, &cmd, &char_id, &name, &members, history_limit)
                .await;
        }
        if cmd.channel == "guild" {
            let guild = db::guilds::guild_of(&self.state.pool, &char_id)
                .await
                .ok()
                .flatten();
            let Some(guild) = guild else {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "вы не в гильдии",
                )];
            };
            let members = db::guilds::members(&self.state.pool, &guild.0)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>();
            return self
                .chat_deliver(seq, &cmd, &char_id, &name, &members, history_limit)
                .await;
        }
        let recipients: Vec<String> = {
            let world = self.state.world.lock().expect("мир");
            match cmd.channel.as_str() {
                "location" => world.chars_in_location(&location),
                "global" | "system" => world.online_chars(),
                "whisper" => {
                    let target = cmd.to.as_deref().and_then(|to| world.char_id_by_name(to));
                    match target {
                        Some(id) if id != char_id => vec![char_id.clone(), id],
                        Some(_) => {
                            return vec![Self::err(
                                seq,
                                ErrorCode::TargetGone,
                                "нельзя шептать себе",
                            )];
                        }
                        None => {
                            return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
                        }
                    }
                }
                _ => vec![char_id.clone()],
            }
        };

        self.chat_deliver(seq, &cmd, &char_id, &name, &recipients, history_limit)
            .await
    }

    /// Общая доставка сообщения: история в БД (кольцо server_history,
    /// D25) → рассылка адресатам → ack (§2.2: ack раньше событий).
    pub(super) async fn chat_deliver(
        &mut self,
        seq: u32,
        cmd: &ChatSend,
        char_id: &str,
        name: &str,
        recipients: &[String],
        history_limit: u32,
    ) -> Vec<Out> {
        let (msg_id, sent_at) = match db::chat::push(
            &self.state.pool,
            &cmd.channel,
            char_id,
            name,
            cmd.text.trim(),
            history_limit,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("chat (история): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        self.last_chat_at = Some(std::time::Instant::now());

        let payload = ld2_protocol::evt::ChatMessage {
            // M14/F19: id строки — для cmd/chat.delete (модерация).
            id: Some(msg_id.max(0) as u64),
            channel: cmd.channel.clone(),
            from: char_id.to_string(),
            name: name.to_string(),
            text: cmd.text.trim().to_string(),
            tags: None,
            sent_at,
            extra: Default::default(),
        };
        let text = event_payload_text("evt/chat/message", &payload);
        if let Some(text) = text {
            let world = self.state.world.lock().expect("мир");
            for addr in recipients {
                world.send_to_char(addr, text.clone());
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// Реплей истории чата при выборе персонажа (§5 `server_history`):
    /// события `evt/chat/message` после snapshot (кольцо переживает рестарт).
    pub(super) async fn chat_history_events(&self) -> Vec<Out> {
        let limit = self.state.config.gameplay.chat.server_history;
        if limit == 0 {
            return Vec::new();
        }
        let rows = match db::chat::recent(&self.state.pool, limit).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("chat (реплей истории): {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter_map(|(id, channel, from, name, text, sent_at)| {
                // M14/F22: реплей помечается — клиент не показывает
                // прошлую сессию новому входу (кольцо D25 в БД живёт,
                // совместимость — через extra §11).
                let mut extra = serde_json::Map::new();
                extra.insert("replay".to_string(), serde_json::Value::Bool(true));
                let payload = ld2_protocol::evt::ChatMessage {
                    // M14/F19: id переживает рестарт — реплей тоже удаляем.
                    id: Some(id.max(0) as u64),
                    channel,
                    from,
                    name,
                    text,
                    tags: None,
                    sent_at,
                    extra,
                };
                event_payload_text("evt/chat/message", &payload).map(Out::Text)
            })
            .collect()
    }
}

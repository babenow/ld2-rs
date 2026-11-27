//! Редьюсеры соцсвязей — чат, пати, торговля, гильдия (RF4g).

use ld2_protocol::names;

use super::super::*;

impl AppStore {
    /// Чат/пати/торговля/гильдия: `evt/chat/*`, `evt/party/updated`, `evt/trade/*`, `evt/guild/*` (RF4g).
    pub(crate) fn reduce_social(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Vec<Effect> {
        match msg_type {
            m if m.starts_with(names::EVT_CHAT_PREFIX) => self.reduce_chat(m, payload),
            names::EVT_PARTY_UPDATED => self.reduce_party(msg_type, payload),
            m if m.starts_with(names::EVT_TRADE_PREFIX) => self.reduce_trade(m, payload),
            m if m.starts_with(names::EVT_GUILD_PREFIX) => self.reduce_guild(m, payload),
            _ => vec![],
        }
    }

    /// Чат: сообщения и удаление.
    fn reduce_chat(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_CHAT_MESSAGE => {
                let Ok(msg) = serde_json::from_value::<ChatMessage>(payload) else {
                    return vec![];
                };
                // M14/F22: реплей истории (маркер replay) не показываем —
                // журнал и чат входят в игру пустыми; кольцо D25 живёт
                // на сервере, совместимость — через extra (§11).
                if msg.extra.get("replay").and_then(|v| v.as_bool()) == Some(true) {
                    return vec![];
                }
                // ФР-13: история per-channel (ring) + адресат для «ответить».
                let my_id = self
                    .snapshot
                    .as_ref()
                    .map(|sn| sn.char.id.clone())
                    .unwrap_or_default();
                let mut effects = vec![];
                if msg.channel == "whisper" && msg.from != my_id {
                    self.last_whisper_from = Some(msg.name.clone());
                    effects.push(Effect::Notify(NotifyKind::Whisper));
                }
                let entry = self.chat.entry(msg.channel.clone()).or_default();
                if entry.len() >= self.chat_limit {
                    entry.pop_front();
                }
                entry.push_back(msg.clone());
                // Заказчик 2026-11-20: в журнале канал — локализованной
                // меткой ([Локация], не [location]) и своим цветом.
                let (kind, label) = match msg.channel.as_str() {
                    "global" => (LogKind::ChatGlobal, t(self.lang, Key::ChGlobal)),
                    "party" => (LogKind::ChatParty, t(self.lang, Key::ChParty)),
                    "guild" => (LogKind::ChatGuild, t(self.lang, Key::ChGuild)),
                    "whisper" => (LogKind::ChatWhisper, t(self.lang, Key::ChWhisper)),
                    "system" => (LogKind::ChatSystem, t(self.lang, Key::ChSystem)),
                    _ => (LogKind::Chat, t(self.lang, Key::ChLocation)),
                };
                self.push_log(kind, format!("[{}] {}: {}", label, msg.name, msg.text));
                effects
            }
            names::EVT_CHAT_DELETED => {
                // M14/F19: модератор удалил сообщение — убираем из всех
                // каналов (id один на историю сервера).
                let Ok(del) = serde_json::from_value::<ld2_protocol::evt::ChatDeleted>(payload)
                else {
                    return vec![];
                };
                for ring in self.chat.values_mut() {
                    ring.retain(|m| m.id != Some(del.id));
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Пати: обновление состава.
    fn reduce_party(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_PARTY_UPDATED => {
                #[derive(serde::Deserialize)]
                struct WithParty {
                    #[serde(default)]
                    party: Option<PartyState>,
                    #[serde(default)]
                    invite: Option<PartyInviteView>,
                }
                let Ok(w) = serde_json::from_value::<WithParty>(payload) else {
                    return vec![];
                };
                // ФР-14: полное состояние группы + входящее приглашение (§11).
                self.party = w.party;
                if let Some(inv) = w.invite {
                    self.push_log(
                        LogKind::System,
                        format!("Приглашение в группу от {}", inv.from_name),
                    );
                    self.party_invite = Some(inv);
                    return vec![Effect::Notify(NotifyKind::PartyInvite)];
                }
                if self.party.is_some() {
                    self.party_invite = None; // состав обновился — ответ дан
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Торговля: состояние сделки и закрытие.
    fn reduce_trade(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_TRADE_STATE => {
                let Ok(trade) = serde_json::from_value::<ld2_protocol::evt::TradeState>(payload)
                else {
                    return vec![];
                };
                // ФР-15: версия/зеркало — истина сервера, клиент не пересчитывает.
                // Открытие сделки партнёром — уведомление (D7).
                let opened = self.trade.is_none();
                self.trade = Some(trade);
                if opened {
                    return vec![Effect::Notify(NotifyKind::TradeOpen)];
                }
                vec![]
            }
            names::EVT_TRADE_CLOSED => {
                #[derive(serde::Deserialize)]
                struct Closed {
                    offer_id: String,
                    reason: ld2_protocol::evt::TradeCloseReason,
                }
                let Ok(c) = serde_json::from_value::<Closed>(payload) else {
                    return vec![];
                };
                self.trade = None;
                let text = match c.reason {
                    ld2_protocol::evt::TradeCloseReason::Done => {
                        format!("Сделка {} состоялась", c.offer_id)
                    }
                    ld2_protocol::evt::TradeCloseReason::Cancelled => {
                        format!("Сделка {} отменена", c.offer_id)
                    }
                    _ => format!("Сделка {} не состоялась", c.offer_id),
                };
                self.push_log(LogKind::Loot, text);
                vec![]
            }
            _ => vec![],
        }
    }

    /// Гильдия: состав и журнал.
    fn reduce_guild(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_GUILD_UPDATED => {
                #[derive(serde::Deserialize)]
                struct WithGuild {
                    #[serde(default)]
                    guild: Option<GuildState>,
                    #[serde(default)]
                    invite: Option<GuildInviteView>,
                }
                let Ok(w) = serde_json::from_value::<WithGuild>(payload) else {
                    return vec![];
                };
                self.guild = w.guild;
                if let Some(inv) = w.invite {
                    self.push_log(
                        LogKind::System,
                        format!("Приглашение в гильдию «{}»", inv.guild_name),
                    );
                    self.guild_invite = Some(inv);
                    return vec![Effect::Notify(NotifyKind::GuildInvite)];
                }
                vec![]
            }
            names::EVT_GUILD_LOG => {
                #[derive(serde::Deserialize)]
                struct WithEntry {
                    entry: GuildLogEntry,
                }
                let Ok(w) = serde_json::from_value::<WithEntry>(payload) else {
                    return vec![];
                };
                // §8: журнал гильдии попадает и в системный лог (ФР-16).
                self.push_log(LogKind::System, format!("Гильдия: {}", w.entry.text));
                self.guild_log.push(w.entry);
                vec![]
            }
            _ => vec![],
        }
    }
}

//! Социальное состояние в памяти (M12.7): группы (ФР-14) — эфемерно
//! (D31: один шард), сделки/гильдии — в этой же структуре (M12.7c/d).
//!
//! Домены разложены по подмодулям: `party`, `trade`, `guild`; общая
//! структура `SocialState` и её индексы — здесь.

use std::collections::HashMap;

mod guild;
mod party;
mod trade;

pub use guild::GuildInvite;
pub use party::InviteError;
pub use trade::{Trade, TradeError, TradeSide};

use ld2_protocol::types::PartyState;

use party::Invite;

/// Максимум участников группы берётся из конфига (`party_max_members`).
#[derive(Default)]
pub struct SocialState {
    /// party_id → группа.
    parties: HashMap<String, PartyState>,
    /// offer_id → сделка (ФР-15, M12.7c).
    trades: HashMap<String, Trade>,
    /// char_id → offer_id активной сделки (индекс для быстрого поиска).
    char_trade: HashMap<String, String>,
    trade_seq: u64,
    /// invite_id → приглашение в гильдию (эфемерно; сама гильдия — в БД).
    guild_invites: HashMap<String, GuildInvite>,
    guild_invite_seq: u64,
    /// char_id → party_id (обратный индекс для быстрого поиска).
    char_party: HashMap<String, String>,
    /// invite_id → (адресат, приглашающий, имя приглашающего, party_id).
    invites: HashMap<String, Invite>,
    party_seq: u64,
    invite_seq: u64,
}

/// Результат операции с группой: события адресатам + (опц.) ответ для автора.
#[derive(Debug, Default)]
pub struct SocialEvents {
    /// (char_id, JSON payload `evt/party/updated`).
    pub party_updates: Vec<(String, serde_json::Value)>,
    /// (char_id, JSON payload `evt/party/updated` с `invite`) — приглашение.
    pub invites: Vec<(String, serde_json::Value)>,
}

//! Команды `cmd/*` — `docs/protocol.md` §7 (payload/ack/idempotency).
//!
//! Каждая команда — структура payload'а + wire-`type` + флаг идемпотентности
//! (строка «Идемп.» §7): транспорт ретраит **только** `IDEMPOTENT = true`
//! (PLAN-M1 T2, protocol.md §2.4).

use serde::{Deserialize, Serialize};

/// Команда: имеет wire-`type` (`cmd/<domain>.<action>`) и сериализуется в payload.
pub trait Command: Serialize {
    const TYPE: &'static str;
    /// Повтор безопасен: ack кэшируется сервером, действие не выполняется дважды.
    const IDEMPOTENT: bool;
}

macro_rules! impl_commands {
    ($(($name:ident, $ty:literal, $idem:literal)),* $(,)?) => {$(
        impl Command for $name {
            const TYPE: &'static str = $ty;
            const IDEMPOTENT: bool = $idem;
        }
    )*};
}

// ─── auth / session (§4.5) ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthLogin {
    pub login: String,
    pub password: String,
}

/// Регистрация с немедленным входом (ADR-006, D28): ack — как у `login`,
/// далее `evt/session.start`. Лимиты длины — настройки сервера.
/// `email` обязателен и уникален (D33, ADR-007): нормализация и валидация
/// — на сервере (`E_BAD_EMAIL`/`E_EMAIL_TAKEN`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthRegister {
    pub login: String,
    pub password: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthResume {
    pub refresh_token: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthLogout {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SessionRefresh {}

// ─── персонажи (ФР-02; profile — D3, ADR-003) ────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CharList {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CharCreate {
    pub name: String,
    /// Стартовый профиль: Воин/Вор/Маг (список — capability сервера).
    pub profile: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CharSelect {
    pub char_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CharDelete {
    pub char_id: String,
}

/// Вложение очков опыта из пула квестов (D3/§2.1 skills.md).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CharXpInvest {
    pub skill_id: String,
    pub points: i64,
}

// ─── мир / навигация (ФР-03) ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LocationLook {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LocationMove {
    pub exit_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EntityInfo {
    pub entity_id: String,
}

// ─── бой (ФР-05) ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CombatAttack {
    pub target_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CombatStop {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CombatFlee {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SkillUse {
    pub skill_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

// ─── предметы (ФР-07) ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemUse {
    pub item_uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemEquip {
    pub item_uid: String,
    pub slot: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemUnequip {
    pub slot: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemDrop {
    pub item_uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty: Option<i64>,
}

/// M16/F29g: поменять местами два предмета рюкзака (перетаскивание)
/// → ack `{}`; порядок — `items.sort_order`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemMove {
    pub item_uid: String,
    pub target_uid: String,
}

// ─── магия (ФР-09; D4: руна = мана + заряд) ──────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MagicScrollCast {
    pub item_uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MagicRuneCast {
    pub staff_uid: String,
    pub socket_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SocketInsert {
    pub staff_uid: String,
    pub socket_index: u32,
    pub rune_uid: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SocketExtract {
    pub staff_uid: String,
    pub socket_index: u32,
}

// ─── улучшение предметов (ФР-10) ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpgradePreview {
    pub item_uid: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpgradeStart {
    pub item_uid: String,
    pub preview_id: String,
}

// ─── профессии/крафт (ФР-08, skills.md) ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CraftList {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CraftRecipes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profession: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CraftLearn {
    pub profession_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CraftStart {
    pub recipe_id: String,
    pub qty: i64,
}

// ─── квесты (ФР-12) ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QuestList {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QuestAccept {
    pub quest_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QuestTurnIn {
    pub quest_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QuestAbandon {
    pub quest_id: String,
}

// ─── преступность (ФР-11) ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CrimeLog {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

// ─── чат (ФР-13) ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChatSend {
    pub channel: String,
    pub text: String,
    /// Адресат для `channel = "whisper"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// M14/F19: удаление сообщения из истории модератором/админом (§7).
/// Права роли проверяет сервер → `E_PERMISSION`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChatDelete {
    /// `id` строки в `chat_history` (BIGSERIAL).
    pub id: u64,
}

// ─── группа (ФР-14) ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartyCreate {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartyInvite {
    pub player_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartyAccept {
    pub invite_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartyDecline {
    pub invite_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartyLeave {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartyKick {
    pub player_id: String,
}

// ─── торговля (ФР-15) ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TradeOpen {
    pub player_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TradeAddItem {
    pub offer_id: String,
    pub item_uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TradeSetGold {
    pub offer_id: String,
    pub gold: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TradePropose {
    pub offer_id: String,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TradeAccept {
    pub offer_id: String,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TradeCancel {
    pub offer_id: String,
}

// ─── гильдии (ФР-16) ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GuildBrowse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GuildCreate {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GuildJoin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GuildLeave {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GuildInvite {
    pub player_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GuildKick {
    pub player_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GuildRankSet {
    pub player_id: String,
    pub rank: String,
}

// ─── состояние / сервис ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StateResync {
    pub last_state_version: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Ping {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TimeSync {}

// ─── wire-типы (protocol.md §7: «Идемп.» = +) ────────────────────────────────

impl_commands!(
    (AuthLogin, "cmd/auth.login", true),
    (AuthRegister, "cmd/auth.register", true),
    (AuthResume, "cmd/auth.resume", true),
    (AuthLogout, "cmd/auth.logout", true),
    (SessionRefresh, "cmd/session.refresh", true),
    (CharList, "cmd/char.list", true),
    (CharCreate, "cmd/char.create", false),
    (CharSelect, "cmd/char.select", true),
    (CharDelete, "cmd/char.delete", false),
    (CharXpInvest, "cmd/char.xp_invest", false),
    (LocationLook, "cmd/location.look", true),
    (LocationMove, "cmd/location.move", false),
    (EntityInfo, "cmd/entity.info", true),
    (CombatAttack, "cmd/combat.attack", false),
    (CombatStop, "cmd/combat.stop", true),
    (CombatFlee, "cmd/combat.flee", false),
    (SkillUse, "cmd/skill.use", false),
    (ItemUse, "cmd/item.use", false),
    (ItemEquip, "cmd/item.equip", true),
    (ItemUnequip, "cmd/item.unequip", true),
    (ItemDrop, "cmd/item.drop", false),
    (ItemMove, "cmd/item.move", false),
    (MagicScrollCast, "cmd/magic.scroll_cast", false),
    (MagicRuneCast, "cmd/magic.rune_cast", false),
    (SocketInsert, "cmd/socket.insert", false),
    (SocketExtract, "cmd/socket.extract", false),
    (UpgradePreview, "cmd/upgrade.preview", true),
    (UpgradeStart, "cmd/upgrade.start", false),
    (CraftList, "cmd/craft.list", true),
    (CraftRecipes, "cmd/craft.recipes", true),
    (CraftLearn, "cmd/craft.learn", false),
    (CraftStart, "cmd/craft.start", false),
    (QuestList, "cmd/quest.list", true),
    (QuestAccept, "cmd/quest.accept", false),
    (QuestTurnIn, "cmd/quest.turn_in", false),
    (QuestAbandon, "cmd/quest.abandon", false),
    (CrimeLog, "cmd/crime.log", true),
    (ChatSend, "cmd/chat.send", false),
    (ChatDelete, "cmd/chat.delete", false),
    (PartyCreate, "cmd/party.create", false),
    (PartyInvite, "cmd/party.invite", false),
    (PartyAccept, "cmd/party.accept", false),
    (PartyDecline, "cmd/party.decline", false),
    (PartyLeave, "cmd/party.leave", true),
    (PartyKick, "cmd/party.kick", false),
    (TradeOpen, "cmd/trade.open", false),
    (TradeAddItem, "cmd/trade.add_item", false),
    (TradeSetGold, "cmd/trade.set_gold", false),
    (TradePropose, "cmd/trade.propose", false),
    (TradeAccept, "cmd/trade.accept", false),
    (TradeCancel, "cmd/trade.cancel", true),
    (GuildBrowse, "cmd/guild.browse", true),
    (GuildCreate, "cmd/guild.create", false),
    (GuildJoin, "cmd/guild.join", false),
    (GuildLeave, "cmd/guild.leave", false),
    (GuildInvite, "cmd/guild.invite", false),
    (GuildKick, "cmd/guild.kick", false),
    (GuildRankSet, "cmd/guild.rank_set", true),
    (StateResync, "cmd/state.resync", true),
    (Ping, "cmd/ping", true),
    (TimeSync, "cmd/time.sync", true),
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_types_follow_conventions() {
        // Сверка с таблицей §7: все типы вида cmd/<domain>.<action>.
        for ty in [
            AuthLogin::TYPE,
            AuthRegister::TYPE,
            CharCreate::TYPE,
            LocationMove::TYPE,
            MagicRuneCast::TYPE,
            GuildRankSet::TYPE,
            StateResync::TYPE,
            TimeSync::TYPE,
        ] {
            assert!(ty.starts_with("cmd/"), "{ty}");
            assert!(ty.contains('.'), "{ty}");
        }
        // Идемпотентность по §7: повторять можно только безопасные команды.
        const { assert!(Ping::IDEMPOTENT) };
        const { assert!(AuthLogin::IDEMPOTENT) };
        // Ретрай register после обрыва не создаёт второй аккаунт (ADR-006).
        const { assert!(AuthRegister::IDEMPOTENT) };
        const { assert!(!LocationMove::IDEMPOTENT) };
        const { assert!(!ChatSend::IDEMPOTENT) };
        // M14/F19: повтор удаления — «не найдено», не кэшируем ack.
        const { assert!(!ChatDelete::IDEMPOTENT) };
        assert!(ChatDelete::TYPE.starts_with("cmd/"));
        assert!(ChatDelete::TYPE.contains('.'));
    }

    #[test]
    fn empty_payload_serializes_as_object() {
        let json = serde_json::to_value(Ping {}).expect("serialize");
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn optional_fields_are_omitted() {
        let json = serde_json::to_value(SkillUse {
            skill_id: "power_strike".into(),
            target_id: None,
        })
        .expect("serialize");
        assert_eq!(json, serde_json::json!({ "skill_id": "power_strike" }));
    }
}

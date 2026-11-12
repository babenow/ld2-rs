//! События `evt/*` — `docs/protocol.md` §8 (сессия/состояние, персонаж и мир,
//! бой и магия, предметы, преступность, социальное).
//!
//! Неизвестный `type` клиент игнорирует с логом (§2.3); новые опциональные
//! поля совместимы (§11).

use serde::{Deserialize, Serialize};

use crate::features::SessionConfig;
use crate::types::{
    CharacterState, CrimeLogEntry, CrimeStatus, Entity, GuildState, Hp, ItemSnapshot, JsonMap,
    LocationSnapshot, PartyState, ProfessionSnapshot, QuestState,
};

/// Событие: wire-`type` (`evt/<domain>.<event>`) и (де)сериализация payload'а.
pub trait Event: Serialize + serde::de::DeserializeOwned {
    const TYPE: &'static str;
}

macro_rules! impl_events {
    ($(($name:ident, $ty:literal)),* $(,)?) => {$(
        impl Event for $name {
            const TYPE: &'static str = $ty;
        }
    )*};
}

// ─── сессия / состояние ──────────────────────────────────────────────────────

impl Event for SessionConfig {
    const TYPE: &'static str = "evt/session.start";
}

/// Полный снапшот (§3): приходит на `char.select` и после `state.resync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub state_version: u64,
    pub char: CharacterState,
    pub location: LocationSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub party: Option<PartyState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild: Option<crate::types::GuildState>,
    /// Epoch-серверного времени (§1.1).
    pub server_ts: i64,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Операция JSON-Patch-like патча (§3): `path` — JSON Pointer от корня снапшота.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchOp {
    pub op: PatchOpKind,
    pub path: String,
    /// Обязателен для `add`/`replace`, отсутствует для `remove`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchOpKind {
    Add,
    Remove,
    Replace,
}

/// Инкрементальное обновление состояния (§3). При несовпадении `path` клиент
/// обязан сброситься на `cmd/state.resync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateDelta {
    pub from_version: u64,
    pub to_version: u64,
    #[serde(default)]
    pub patch: Vec<PatchOp>,
}

/// Эхо `cmd/ping` (§7): `ts` — ISO-серверное время для расчёта RTT.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    pub ts: String,
    pub rtt_ms: u64,
}

/// Калибровка часов клиента (§1.1): `server_ts` — epoch-секунды.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeSync {
    pub server_ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_hint_ms: Option<u64>,
}

/// Серверное уведомление вне ack (§8): `fatal = true` → клиент к переподключению.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorNotice {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fatal: Option<bool>,
}

// ─── персонаж и мир ──────────────────────────────────────────────────────────

/// Начисление XP навыку (§8; `pool_left` — D4: остаток пула после вложения).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharXp {
    pub skill_id: String,
    pub amount: i64,
    pub skill_xp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xp_to_next: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_left: Option<i64>,
}

/// Патч атрибутов/HP/MP (§8): открытый объект, сервер — истина.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharStats {
    pub patch: JsonMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LostItem {
    pub uid: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rarity: Option<String>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Respawn {
    pub location_id: String,
    pub delay_s: u64,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Смерть ПЕРСОНАЖА: полная потеря лута (ФР-06). Окно «Вы погибли».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharDeath {
    pub victim_char: bool,
    #[serde(default)]
    pub lost_items: Vec<LostItem>,
    pub corpse: String,
    pub respawn: Respawn,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocationChanged {
    pub location: LocationSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntitySpawn {
    pub entity: Entity,
    pub location_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityMove {
    pub entity_id: String,
    pub patch: JsonMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityHp {
    pub entity_id: String,
    pub hp: Hp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityDespawn {
    pub entity_id: String,
    pub location_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpseCreated {
    pub entity: Entity,
    pub loot_count: u32,
}

/// Лут собран; `was_player` → UI предупреждает о статусе «Мародёр» (ФР-11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpseLooted {
    pub corpse_id: String,
    pub by: String,
    pub by_name: String,
    pub was_player: bool,
}

// ─── бой и магия ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatKind {
    Melee,
    Skill,
    Rune,
    Scroll,
    #[serde(other)]
    Unknown,
}

/// Удар (§8): `text` — серверный, рендерится как есть (локализация на сервере).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombatHit {
    pub source: String,
    pub source_name: String,
    pub target: String,
    pub target_name: String,
    pub kind: CombatKind,
    pub amount: i64,
    pub crit: bool,
    pub miss: bool,
    pub text: String,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Блокировка действия до `until_epoch` (epoch-секунды, §1.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombatCooldown {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    pub until_epoch: i64,
    /// M17/F30g: полная длительность кулдауна, мс — для прогресс-бара
    /// клиента (`None` — старый сервер, без бара).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombatEnd {
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CastSource {
    Scroll,
    Rune,
}

/// Каст (свой/чужой). D4: `charges_left` — заряды руны, `mp_left` — остаток маны.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagicCast {
    pub by: String,
    pub by_name: String,
    pub spell: String,
    pub what: CastSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charges_left: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mp_left: Option<i32>,
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

// ─── предметы ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemAdded {
    pub item: ItemSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemRemovalReason {
    Consumed,
    Traded,
    Dropped,
    LostOnDeath,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemRemoved {
    pub uid: String,
    pub reason: ItemRemovalReason,
}

/// Патч предмета (`upgrade`, `sockets`, `qty`) — ФР-09/ФР-10.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemState {
    pub uid: String,
    pub patch: JsonMap,
}

/// Итог улучшения (ФР-10): `cost` — открытый объект (gold/materials).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpgradeResult {
    pub uid: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_upgrade: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_destroyed: Option<bool>,
    #[serde(default)]
    pub cost: JsonMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MadeItem {
    pub uid: String,
    pub qty: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CraftResult {
    pub recipe_id: String,
    pub ok: bool,
    #[serde(default)]
    pub made: Vec<MadeItem>,
    #[serde(default)]
    pub consumed: Vec<serde_json::Value>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Профессия изучена/ранг повышен (§8; лимит — `profession_limit`, D10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CraftProfession {
    pub profession: ProfessionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestProgress {
    pub quest: QuestState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestUpdated {
    pub quest: QuestState,
}

// ─── преступность (§10, ФР-11) ───────────────────────────────────────────────

/// **Полная замена** списка активных статусов (клиент не пересчитывает blend).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrimeStatusUpdate {
    #[serde(default)]
    pub statuses: Vec<CrimeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_hint: Option<String>,
}

/// Новый срок: `log_entry` дополняет журнал нарушений.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrimeAdded {
    pub status: CrimeStatus,
    pub log_entry: CrimeLogEntry,
}

/// Статус истёк; fallback — локальный снимок по `expires_at` (ФР-11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrimeRemoved {
    pub kind: String,
}

// ─── социальное ──────────────────────────────────────────────────────────────

/// Сообщение чата (§8, ФР-13): `sent_at` — ISO-8601 UTC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// M14/F19: id строки в истории (для удаления модератором);
    /// у старых серверов отсутствует → `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub channel: String,
    pub from: String,
    pub name: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    pub sent_at: String,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// M14/F19: модератор удалил сообщение — клиент убирает строку по `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatDeleted {
    pub id: u64,
}

/// Полное состояние группы; `party: null` — группы нет (§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartyUpdated {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub party: Option<PartyState>,
}

/// Состояние сделки; каждое изменение = новый `version` (ФР-15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeState {
    pub offer_id: String,
    pub version: u64,
    #[serde(default)]
    pub you: Vec<ItemSnapshot>,
    #[serde(default)]
    pub them: Vec<ItemSnapshot>,
    pub you_gold: i64,
    pub them_gold: i64,
    pub you_locked: bool,
    pub them_locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_hint: Option<i64>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeCloseReason {
    Done,
    Cancelled,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeClosed {
    pub offer_id: String,
    pub reason: TradeCloseReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Полное состояние гильдии; `guild: null` — гильдии нет (§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildUpdated {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild: Option<GuildState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildLogEntry {
    /// Тип события: join/leave/rank_change/… (открытая строка).
    pub kind: String,
    pub text: String,
    /// Epoch-секунды.
    pub at: i64,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildLog {
    pub entry: GuildLogEntry,
}

// ─── wire-типы (protocol.md §8) ──────────────────────────────────────────────

impl_events!(
    (StateSnapshot, "evt/state/snapshot"),
    (StateDelta, "evt/state/delta"),
    (Pong, "evt/pong"),
    (TimeSync, "evt/time.sync"),
    (ErrorNotice, "evt/error"),
    (CharXp, "evt/char/xp"),
    (CharStats, "evt/char/stats"),
    (CharDeath, "evt/char/death"),
    (LocationChanged, "evt/location/changed"),
    (EntitySpawn, "evt/entity/spawn"),
    (EntityMove, "evt/entity/move"),
    (EntityHp, "evt/entity/hp"),
    (EntityDespawn, "evt/entity/despawn"),
    (CorpseCreated, "evt/corpse/created"),
    (CorpseLooted, "evt/corpse/looted"),
    (CombatHit, "evt/combat/hit"),
    (CombatCooldown, "evt/combat/cooldown"),
    (CombatEnd, "evt/combat/end"),
    (MagicCast, "evt/magic/cast"),
    (ItemAdded, "evt/item/added"),
    (ItemRemoved, "evt/item/removed"),
    (ItemState, "evt/item/state"),
    (UpgradeResult, "evt/upgrade/result"),
    (CraftResult, "evt/craft/result"),
    (CraftProfession, "evt/craft/profession"),
    (QuestProgress, "evt/quest/progress"),
    (QuestUpdated, "evt/quest/updated"),
    (CrimeStatusUpdate, "evt/crime/status"),
    (CrimeAdded, "evt/crime/added"),
    (CrimeRemoved, "evt/crime/removed"),
    (ChatMessage, "evt/chat/message"),
    (ChatDeleted, "evt/chat/deleted"),
    (PartyUpdated, "evt/party/updated"),
    (TradeState, "evt/trade/state"),
    (TradeClosed, "evt/trade/closed"),
    (GuildUpdated, "evt/guild/updated"),
    (GuildLog, "evt/guild/log"),
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SkillSnapshot;

    #[test]
    fn wire_types_follow_conventions() {
        for ty in [
            StateSnapshot::TYPE,
            CharXp::TYPE,
            MagicCast::TYPE,
            CrimeStatusUpdate::TYPE,
            GuildLog::TYPE,
            SessionConfig::TYPE,
            ChatDeleted::TYPE,
        ] {
            assert!(ty.starts_with("evt/"), "{ty}");
        }
        assert_eq!(ChatDeleted::TYPE, "evt/chat/deleted");
    }

    #[test]
    fn patch_op_omits_value_on_remove() {
        let op = PatchOp {
            op: PatchOpKind::Remove,
            path: "/location/entities/m_3".into(),
            value: None,
        };
        let json = serde_json::to_value(op).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({ "op": "remove", "path": "/location/entities/m_3" })
        );
    }

    #[test]
    fn skill_snapshot_used_in_char_xp_docs() {
        // Документ-ссылка: SkillSnapshot — общий тип (§6), не ломается.
        let s = SkillSnapshot {
            id: "sword".into(),
            name: None,
            description: None,
            group: None,
            rank: 3,
            xp: 612,
            xp_to_next: 480,
        };
        assert_eq!(serde_json::to_value(s).expect("ok")["rank"], 3);
    }
}

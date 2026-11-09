//! `evt/session.start` — конфиг аккаунта и capabilities (`docs/protocol.md` §5).
//!
//! Правило (ТЗ §15.4): клиент берёт **все** лимиты/фичи отсюда; незнакомые ключи
//! (в т.ч. в `features` и `extra`) игнорируются, хардкоды вида `MAX_CHARS` запрещены.

use serde::{Deserialize, Serialize};

use crate::types::{CharacterSummary, JsonMap, Severity};

// ─── §5: пример payload зафиксирован golden-тестом (tests/golden.rs) ─────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub server: ServerInfo,
    pub session: SessionTokens,
    pub char_slot_limit: u32,
    #[serde(default)]
    pub profession_limit: u32,
    /// M14/F19: роль аккаунта — `player` | `admin` | `moderator`
    /// (модерация чата); старый сервер без поля → `player`.
    #[serde(default = "default_account_role")]
    pub role: String,
    /// Capability-фичи с точечными ключами (`loot.roll`, `upgrade.rules`, …).
    #[serde(default)]
    pub features: JsonMap,
    pub crime: CrimeConfig,
    pub chat: ChatConfig,
    pub inventory: InventoryConfig,
    pub upgrade: UpgradeConfig,
    pub magic: MagicConfig,
    #[serde(default)]
    pub characters: Vec<CharacterSummary>,
    /// Незнакомые поля — сохраняем и игнорируем (§2.3, §11).
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Роль по умолчанию для серверов без поля (§11 обратная совместимость).
fn default_account_role() -> String {
    "player".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Минимальная версия клиента; `null` — не ограничено (§5, §11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_client: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTokens {
    pub access_token: String,
    pub expires_in_s: u64,
    pub refresh: RefreshToken,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshToken {
    pub token: String,
    pub expires_in_s: u64,
}

/// Конфиг преступности: формула «частичного сложения» (§10) — серверная,
/// клиент только рендерит. `base` — открытые ключи `<kind>_s`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrimeConfig {
    pub t_max_s: i64,
    pub blend_k: f64,
    #[serde(default)]
    pub base: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub kinds: Vec<CrimeKind>,
}

/// Вид статуса: клиент рендерит любые `kind` через title/severity (§5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrimeKind {
    pub kind: String,
    pub title: String,
    pub severity: Severity,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatConfig {
    pub server_history: u32,
    #[serde(default)]
    pub channels: Vec<String>,
    pub max_len: u32,
    pub cooldown_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryConfig {
    pub slots: u32,
    #[serde(default)]
    pub equip_slots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpgradeConfig {
    pub max_level: u32,
    pub preview: bool,
    pub shows_chance: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagicConfig {
    /// Способ восстановления зарядов рун: `item` (масло алхимика), … (D4).
    pub rune_recharge: String,
    pub scroll_requires_target: bool,
    pub mana: ManaConfig,
}

/// Реген маны (D4, §5): вне боя — секунд на полный бар, в бою — %/сек.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManaConfig {
    pub regen_out_s: u64,
    pub regen_in_pct_s: f64,
    /// Стоимость каста руны в мане (ФР-09: тултип hotbar).
    /// 0/отсутствие — стоимость неизвестна, тултип скрывает (аддитивная
    /// совместимость со старыми серверами, §11).
    #[serde(default)]
    pub rune_cost: i64,
}

impl SessionConfig {
    /// Capability-фича по ключу (`"loot.roll"` и т.п.); `None` — сервер не
    /// прислал ключ (клиент трактует как «выключено», §11).
    pub fn feature(&self, key: &str) -> Option<&serde_json::Value> {
        self.features.get(key)
    }

    /// Фича-булево (`features["loot.roll"] = false` → `Some(false)`).
    pub fn feature_bool(&self, key: &str) -> Option<bool> {
        self.feature(key).and_then(serde_json::Value::as_bool)
    }
}

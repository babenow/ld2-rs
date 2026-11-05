//! Ошибки протокола: `docs/protocol.md` §9 (каталог `ack.error.code`) +
//! ошибки кодирования конверта.

use std::str::FromStr;

use thiserror::Error;

/// Ошибки уровня сериализации/лимитов конверта (не игровые `error.code`).
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("сериализация/десериализация: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("сообщение превышает лимит 64 KiB: {0} байт")]
    TooLarge(usize),
    #[error("нет поля payload или его тип не совпадает с ожидаемым")]
    MissingPayload,
}

/// Игровые коды ошибок `ack.error.code` (protocol.md §9).
///
/// Неизвестный код сервера → [`ErrorCode::Unknown`] (клиент не падает, §2.3),
/// отображение — по русскому `message` из ack.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    AuthExpired,
    BadCredentials,
    /// Логин занят (`auth.register`, ADR-006).
    LoginTaken,
    /// Логин/пароль короче лимитов сервера (`auth.register`, ADR-006).
    WeakPassword,
    /// Регистрация выключена настройкой сервера (ADR-006).
    RegisterDisabled,
    CharLimit,
    NameTaken,
    BadProfile,
    NotInLocation,
    ExitBlocked,
    TargetGone,
    NotInCombat,
    InCombat,
    Cooldown,
    NoCharges,
    NotEnoughMp,
    NoRuneSlot,
    RequirementAttr,
    Profession,
    InventoryFull,
    ItemGone,
    UpgradePreviewStale,
    UpgradeMax,
    TradeStale,
    TradePartnerGone,
    PartyFull,
    GuildLimit,
    GuildPermission,
    XpPool,
    RankCap,
    ProfessionLimit,
    StationRequired,
    ToolMissing,
    Ingredients,
    RateLimited,
    PvpForbidden,
    TooLarge,
    Internal,
    /// Код, неизвестный этой версии клиента (новее контракта / расширение сервера).
    Unknown(String),
}

impl ErrorCode {
    /// Строковый код в wire-формате (`E_...`).
    pub fn as_str(&self) -> &str {
        match self {
            Self::AuthExpired => "E_AUTH_EXPIRED",
            Self::BadCredentials => "E_BAD_CREDENTIALS",
            Self::LoginTaken => "E_LOGIN_TAKEN",
            Self::WeakPassword => "E_WEAK_PASSWORD",
            Self::RegisterDisabled => "E_REGISTER_DISABLED",
            Self::CharLimit => "E_CHAR_LIMIT",
            Self::NameTaken => "E_NAME_TAKEN",
            Self::BadProfile => "E_BAD_PROFILE",
            Self::NotInLocation => "E_NOT_IN_LOCATION",
            Self::ExitBlocked => "E_EXIT_BLOCKED",
            Self::TargetGone => "E_TARGET_GONE",
            Self::NotInCombat => "E_NOT_IN_COMBAT",
            Self::InCombat => "E_IN_COMBAT",
            Self::Cooldown => "E_COOLDOWN",
            Self::NoCharges => "E_NO_CHARGES",
            Self::NotEnoughMp => "E_NOT_ENOUGH_MP",
            Self::NoRuneSlot => "E_NO_RUNE_SLOT",
            Self::RequirementAttr => "E_REQUIREMENT_ATTR",
            Self::Profession => "E_PROFESSION",
            Self::InventoryFull => "E_INVENTORY_FULL",
            Self::ItemGone => "E_ITEM_GONE",
            Self::UpgradePreviewStale => "E_UPGRADE_PREVIEW_STALE",
            Self::UpgradeMax => "E_UPGRADE_MAX",
            Self::TradeStale => "E_TRADE_STALE",
            Self::TradePartnerGone => "E_TRADE_PARTNER_GONE",
            Self::PartyFull => "E_PARTY_FULL",
            Self::GuildLimit => "E_GUILD_LIMIT",
            Self::GuildPermission => "E_GUILD_PERMISSION",
            Self::XpPool => "E_XP_POOL",
            Self::RankCap => "E_RANK_CAP",
            Self::ProfessionLimit => "E_PROFESSION_LIMIT",
            Self::StationRequired => "E_STATION_REQUIRED",
            Self::ToolMissing => "E_TOOL_MISSING",
            Self::Ingredients => "E_INGREDIENTS",
            Self::RateLimited => "E_RATE_LIMITED",
            Self::PvpForbidden => "E_PVP_FORBIDDEN",
            Self::TooLarge => "E_TOO_LARGE",
            Self::Internal => "E_INTERNAL",
            Self::Unknown(raw) => raw,
        }
    }
}

impl FromStr for ErrorCode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "E_AUTH_EXPIRED" => Self::AuthExpired,
            "E_BAD_CREDENTIALS" => Self::BadCredentials,
            "E_LOGIN_TAKEN" => Self::LoginTaken,
            "E_WEAK_PASSWORD" => Self::WeakPassword,
            "E_REGISTER_DISABLED" => Self::RegisterDisabled,
            "E_CHAR_LIMIT" => Self::CharLimit,
            "E_NAME_TAKEN" => Self::NameTaken,
            "E_BAD_PROFILE" => Self::BadProfile,
            "E_NOT_IN_LOCATION" => Self::NotInLocation,
            "E_EXIT_BLOCKED" => Self::ExitBlocked,
            "E_TARGET_GONE" => Self::TargetGone,
            "E_NOT_IN_COMBAT" => Self::NotInCombat,
            "E_IN_COMBAT" => Self::InCombat,
            "E_COOLDOWN" => Self::Cooldown,
            "E_NO_CHARGES" => Self::NoCharges,
            "E_NOT_ENOUGH_MP" => Self::NotEnoughMp,
            "E_NO_RUNE_SLOT" => Self::NoRuneSlot,
            "E_REQUIREMENT_ATTR" => Self::RequirementAttr,
            "E_PROFESSION" => Self::Profession,
            "E_INVENTORY_FULL" => Self::InventoryFull,
            "E_ITEM_GONE" => Self::ItemGone,
            "E_UPGRADE_PREVIEW_STALE" => Self::UpgradePreviewStale,
            "E_UPGRADE_MAX" => Self::UpgradeMax,
            "E_TRADE_STALE" => Self::TradeStale,
            "E_TRADE_PARTNER_GONE" => Self::TradePartnerGone,
            "E_PARTY_FULL" => Self::PartyFull,
            "E_GUILD_LIMIT" => Self::GuildLimit,
            "E_GUILD_PERMISSION" => Self::GuildPermission,
            "E_XP_POOL" => Self::XpPool,
            "E_RANK_CAP" => Self::RankCap,
            "E_PROFESSION_LIMIT" => Self::ProfessionLimit,
            "E_STATION_REQUIRED" => Self::StationRequired,
            "E_TOOL_MISSING" => Self::ToolMissing,
            "E_INGREDIENTS" => Self::Ingredients,
            "E_RATE_LIMITED" => Self::RateLimited,
            "E_PVP_FORBIDDEN" => Self::PvpForbidden,
            "E_TOO_LARGE" => Self::TooLarge,
            "E_INTERNAL" => Self::Internal,
            other => Self::Unknown(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Коды из protocol.md §9 — сверка «документ ↔ типы» (PLAN-M1 T1).
    const DOC_CODES: &[&str] = &[
        "E_AUTH_EXPIRED",
        "E_BAD_CREDENTIALS",
        "E_LOGIN_TAKEN",
        "E_WEAK_PASSWORD",
        "E_REGISTER_DISABLED",
        "E_CHAR_LIMIT",
        "E_NAME_TAKEN",
        "E_BAD_PROFILE",
        "E_NOT_IN_LOCATION",
        "E_EXIT_BLOCKED",
        "E_TARGET_GONE",
        "E_NOT_IN_COMBAT",
        "E_IN_COMBAT",
        "E_COOLDOWN",
        "E_NO_CHARGES",
        "E_NOT_ENOUGH_MP",
        "E_NO_RUNE_SLOT",
        "E_REQUIREMENT_ATTR",
        "E_PROFESSION",
        "E_INVENTORY_FULL",
        "E_ITEM_GONE",
        "E_UPGRADE_PREVIEW_STALE",
        "E_UPGRADE_MAX",
        "E_TRADE_STALE",
        "E_TRADE_PARTNER_GONE",
        "E_PARTY_FULL",
        "E_GUILD_LIMIT",
        "E_GUILD_PERMISSION",
        "E_XP_POOL",
        "E_RANK_CAP",
        "E_PROFESSION_LIMIT",
        "E_STATION_REQUIRED",
        "E_TOOL_MISSING",
        "E_INGREDIENTS",
        "E_RATE_LIMITED",
        "E_PVP_FORBIDDEN",
        "E_TOO_LARGE",
        "E_INTERNAL",
    ];

    #[test]
    fn every_doc_code_parses_and_round_trips() {
        for code in DOC_CODES {
            let parsed: ErrorCode = code.parse().expect("код из документа обязан парситься");
            assert_eq!(parsed.as_str(), *code, "round-trip {code}");
        }
    }

    #[test]
    fn unknown_code_is_forward_compatible() {
        let parsed: ErrorCode = "E_SOME_FUTURE_CODE".parse().expect("не падает");
        assert_eq!(parsed, ErrorCode::Unknown("E_SOME_FUTURE_CODE".into()));
        assert_eq!(parsed.as_str(), "E_SOME_FUTURE_CODE");
    }
}

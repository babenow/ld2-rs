//! Локализация UI: таблицы `ru`/`en` с первого коммита (решение D8, ADR-003).
//!
//! Согласованность языков гарантируется компилятором: `t()` — полный `match`
//! по `Key` в каждом языковом модуле, пропуск ключа = ошибка сборки.
//! Игровые тексты (бойлог, описания локаций) приходят с сервера и не локализуются.

pub mod en;
pub mod ru;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    pub fn from_code(code: &str) -> Self {
        match code {
            "en" => Self::En,
            _ => Self::Ru,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Ru => Self::En,
            Self::En => Self::Ru,
        }
    }
}

/// Строковые ключи UI (M0-шелл + M2: аккаунт/персонажи).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    AppTitle,
    Subtitle,
    Exit,
    Language,
    Host,
    Version,
    ConfigPath,
    DataPath,
    M0Title,
    M0Hint,
    // M2: вход (ФР-01)
    LoginTitle,
    FieldLogin,
    FieldPassword,
    BtnLogin,
    BtnLogout,
    StatusConnecting,
    StatusOnline,
    StatusReconnecting,
    StatusAuthExpired,
    StatusStopped,
    ErrFillFields,
    // M13: регистрация (D35)
    RegisterTitle,
    RegisterSubtitle,
    FieldEmail,
    FieldPassword2,
    BtnRegister,
    RegisterSending,
    ErrRegisterFill,
    ErrPasswordMismatch,
    ErrBadEmail,
    ErrRetryAfter,
    // M2: персонажи (ФР-02)
    CharactersTitle,
    CharactersEmpty,
    SlotsUsed,
    SlotsFull,
    BtnEnter,
    BtnCreate,
    BtnDelete,
    BtnSwitchChar,
    CreateTitle,
    FieldCharName,
    NameTooShort,
    ProfileLabel,
    ProfileWarrior,
    ProfileArcher,
    ProfileMage,
    ProfileHint,
    DeleteTitle,
    DeleteConfirm,
    DeleteTypeNick,
    DeleteTimerLabel,
    DeleteIn,
    DeleteCancelBtn,
    DeleteGameHint,
    Loading,
    GamePlaceholder,
    LocationLabel,
    HpLabel,
    MpLabel,
    XpPoolLabel,
    // M3: мир (ФР-03, ФР-17)
    ExitsLabel,
    PlayersLabel,
    MonstersLabel,
    NpcsLabel,
    CorpsesLabel,
    HistoryLabel,
    LogLabel,
    Examine,
    Attack,
    EntityDetailTitle,
    AltNavHint,
    LootLabel,
    NoExits,
    CorpsePlayerHint,
    // M4: лист/инвентарь/смерть (ФР-04, ФР-06, ФР-07)
    SheetSkills,
    SheetAttrs,
    // M17/F30d: основные характеристики (харизма убрана)
    AttrStr,
    AttrDex,
    AttrVit,
    AttrInt,
    SheetInventory,
    MaxLabel,
    EquipLabel,
    EmptySlot,
    MenuUse,
    MenuEquip,
    MenuUnequip,
    MenuDrop,
    MenuDesc,
    DeathTitle,
    DeathLost,
    DeathConfirm,
    ItemDescTitle,
    ReqLabel,
    // M5: hotbar/улучшение/инкрустация (ФР-09, ФР-10)
    HotbarLabel,
    NoCharge,
    NotEnoughMp,
    UpgradeTitle,
    UpgradeBtn,
    ChanceLabel,
    CostLabel,
    BtnSocketing,
    SocketingTitle,
    InsertRune,
    ExtractRune,
    NoRunes,
    // M6: бой и статусы (ФР-05, ФР-11)
    TargetLabel,
    BtnFlee,
    TabHint,
    PowerStrike,
    StatusesTab,
    CrimeJournal,
    ReloadJournal,
    TotalLabel,
    TakeLoot,
    LootPlayerWarn,
    NoStatuses,
    JournalEmpty,
    DurHm,
    DurMs,
    DurS,
    // M7: квесты и профессии (ФР-12, ФР-08)
    QuestsTab,
    ProfessionsTab,
    TrackerLabel,
    TrackBtn,
    TrackedBtn,
    RewardLabel,
    BtnAccept,
    BtnTurnIn,
    BtnAbandon,
    StateAvailable,
    StateActive,
    StateTurnIn,
    StateDone,
    RecipesLabel,
    BtnCraft,
    LearnLabel,
    NoRecipes,
    NoQuests,
    MissingIngredients,
    RankFmt,
    ProfBlacksmith,
    ProfAlchemist,
    ProfScribe,
    ProfWoodcarver,
    ProfTailor,
    ProfCook,
    ProfHerbalist,
    ProfHunter,
    // M8: чат (ФР-13)
    ChLabel,
    ChSend,
    ChReply,
    ChFilter,
    ChEmpty,
    ChUnknownCmd,
    ChSystem,
    ChLocation,
    ChGlobal,
    ChParty,
    ChGuild,
    ChWhisper,
    // M8: группа, сделка, гильдия (ФР-14…16)
    PartyLabel,
    PartyCreateBtn,
    PartyInviteBtn,
    PartyKickBtn,
    PartyLeaveBtn,
    PartyLeader,
    PartyInviteFrom,
    PartyAcceptBtn,
    PartyDeclineBtn,
    PartyEmpty,
    TradeLabel,
    TradeYou,
    TradeThem,
    TradeGold,
    TradeSetGold,
    TradePropose,
    TradeAccept,
    TradeCancel,
    TradeAddItem,
    TradeValue,
    TradeWarn,
    TradeBtn,
    GuildTab,
    GuildNone,
    GuildCreateLbl,
    GuildCreateBtn,
    GuildJoinBtn,
    GuildLeaveBtn,
    GuildInviteBtn,
    GuildKickBtn,
    GuildRankLbl,
    GuildMembers,
    GuildBrowseLbl,
    GuildLogLbl,
    // M9: настройки (ФР-19)
    SettingsTitle,
    SettingsTheme,
    ThemeDark,
    ThemeLight,
    SettingsFont,
    SettingsChatLimit,
    SettingsNotify,
    SettingsHost,
    SettingsApply,
    SettingsWsWarn,
    SettingsWsDenied,
    SettingsHotkeys,
    HkNavUp,
    HkNavDown,
    HkNavLeft,
    HkNavRight,
    HkCycleTarget,
    HkPress,
    SettingsBtn,
    UpdateBadge,
    On,
    Off,
    BtnClose,
    NotifyDeath,
    NotifyPartyInvite,
    NotifyGuildInvite,
    NotifyTradeOpen,
    NotifyWhisper,
    NotifyUpgrade,
    // M14 F1: шапка окна (бейджи локации)
    BadgePvp,
    BadgeSafe,
    // M14 F16: диалог таймера выхода
    ExitCountdown,
    ExitNowBtn,
    CancelBtn,
    // M14 F17: карточка навыка (иконка ⓘ)
    SkillInfoBtn,
    // M14 F21: предупреждения экрана входа (Caps/раскладка)
    WarnCaps,
    WarnRuLayout,
    // M14 F19: роли и модерация чата
    RolePlayer,
    RoleAdmin,
    RoleModerator,
    SystemChatLocked,
    ChDelete,
    // M14 F4: панель действий и хоткеи
    HkCleave,
    HkRune,
    // ── M15/F25: окно персонажа ──
    /// Заголовок кнопки/окна «Персонаж».
    BtnCharacter,
    /// Вкладка «Параметры» окна персонажа.
    SheetParams,
    /// Вкладка «Репутация» окна персонажа.
    SheetReputation,
    /// Строка хоткея «окно персонажа» в настройках.
    HkOpenCharacter,
    /// «Уровень {n}» — шкала опыта (tf).
    CharLevel,
    /// «Всего опыта» под шкалой.
    CharXpTotal,
    /// «Очки опыта» — свободный пул (D3/D4).
    CharXpPool,
    /// Заглушка вкладки, которую ещё не сделали (F26/F27).
    CharComingSoon,
    // ── M15/F26: вкладка «Параметры» (16 строк) ──
    ParAccuracy,
    ParDamage,
    ParCritChance,
    ParCritDamage,
    ParArmor,
    ParDodge,
    ParParry,
    ParParryChance,
    ParBlock,
    ParBlockChance,
    ParMagicDefense,
    ParMagicDodge,
    ParMagicResist,
    ParMagicDispel,
    ParMagicCritDamage,
    ParMagicCritChance,
    // ── M15/F28: группы навыков (skills.md §3) ──
    SkillGroupWeapon,
    SkillGroupDefense,
    SkillGroupMagic,
    SkillGroupStealth,
    SkillGroupPractice,
    SkillGroupSocial,

    HkConsumable,
    HotbarNoTarget,
    HotbarCooldown,
    HotbarCdShort,
    // M14 F3: инвентарь/скроллы
    InventoryEmpty,
    // M14 F8: настройки шрифтов чата/лога
    SettingsChatFont,
    SettingsLogFont,
    // M14 F9: вкладка настроек
    SettingsGeneral,
    // M14 F11: вкладка «Подключение»
    SettingsConnection,
    SettingsChangeHost,
    // M14 F13: стиль строк боя в журнале
    SettingsLogDamageColor,
    SettingsLogAttackColor,
    SettingsDamageBold,
    // M14 F14: вкладка «Шрифты»
    SettingsFonts,
    // M16/F29g: вкладка «Все предметы» рюкзака (первая)
    BpTabAll,
    // M16/F29f: вкладка «Броня» рюкзака
    BpTabArmor,
    // M16/F29j: чекбокс «Запомнить пароль» на экране входа
    RememberPassword,
    // M16/F29e: переводы характеристик, редкостей, слотов и типов
    StatDamage,
    StatHeal,
    StatArmor,
    StatMagic,
    StatSpell,
    StatSchool,
    StatCharges,
    StatMax,
    SchoolFire,
    SchoolFrost,
    SchoolStorm,
    RarityCommon,
    RarityUncommon,
    RarityRare,
    RarityEpic,
    RarityLegendary,
    SlotHead,
    SlotBody,
    SlotMainHand,
    SlotStaff,
    SlotFeet,
    // M17/F30b: новые слоты и блокировка левой руки
    SlotShoulders,
    SlotBracers,
    SlotGloves,
    SlotLegs,
    SlotCloak,
    SlotRing1,
    SlotRing2,
    SlotAmulet,
    SlotOffHand,
    SlotArtifact1,
    SlotArtifact2,
    SlotBlocked,
    ItemTypeWeapon,
    ItemTypeStaff,
    ItemTypePotion,
    ItemTypeRune,
    ItemTypeScroll,
    ItemTypeArmor,
    ItemTypeMoney,
    ItemTypeHerb,
    ItemTypeWoodOak,
    ItemTypeParchment,
    ItemTypeInk,
    ItemTypeFlask,
    ItemTypeChisel,
    ItemTypeQuill,
    // M17/F30b: новые типы предметов
    ItemTypeShield,
    ItemTypeArtifact,
    // M16/F29d: группа «Атака» вкладки «Параметры»
    ParGroupAttack,
    // M16/F29b: окно рюкзака и его хоткей
    BackpackTitle,
    BpTabWeapon,
    BpTabPotion,
    BpTabMagic,
    BpTabMaterial,
    BpTabTool,
    BpTabOther,
    // M17/F30b: вкладка артефактов рюкзака
    BpTabArtifact,
    // M17/F30a: инкрустация руны из контекстного меню + каст-бар
    MenuInscribe,
    InscribePick,
    InscribeCasting,
    InscribeNoWeapons,
    BpCurrencies,
    HkOpenBackpack,
}

pub fn t(lang: Lang, key: Key) -> &'static str {
    match lang {
        Lang::Ru => ru::t(key),
        Lang::En => en::t(key),
    }
}

/// Строка с подстановкой плейсхолдеров `{name}`:
/// `tf(lang, Key::SlotsUsed, &[("used", "2"), ("limit", "8")])`.
/// Форматирование длительности (ФР-11/НФР): «{h}ч{m}м» / «{m}м{s}с» / «{s}с».
pub fn format_duration(lang: crate::l10n::Lang, secs: i64) -> String {
    use crate::l10n::{tf, Key};
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        tf(
            lang,
            Key::DurHm,
            &[("h", &h.to_string()), ("m", &m.to_string())],
        )
    } else if m > 0 {
        tf(
            lang,
            Key::DurMs,
            &[("m", &m.to_string()), ("s", &s.to_string())],
        )
    } else {
        tf(lang, Key::DurS, &[("s", &s.to_string())])
    }
}

/// Округление вверх — для обратных отсчётов статусов.
pub fn format_duration_ceil(lang: crate::l10n::Lang, secs: i64) -> String {
    use crate::l10n::{tf, Key};
    if secs <= 0 {
        return tf(lang, Key::DurS, &[("s", "0")]);
    }
    let mins = (secs + 59) / 60;
    if mins >= 60 {
        tf(
            lang,
            Key::DurHm,
            &[
                ("h", &(mins / 60).to_string()),
                ("m", &(mins % 60).to_string()),
            ],
        )
    } else {
        tf(lang, Key::DurMs, &[("m", &mins.to_string()), ("s", "0")])
    }
}

/// Локализованное тело OS-уведомления (D7) — используется обоими клиентами.
pub fn notify_body(lang: Lang, kind: crate::store::NotifyKind) -> String {
    use crate::store::NotifyKind::*;
    let key = match kind {
        Death => Key::NotifyDeath,
        PartyInvite => Key::NotifyPartyInvite,
        GuildInvite => Key::NotifyGuildInvite,
        TradeOpen => Key::NotifyTradeOpen,
        Whisper => Key::NotifyWhisper,
        Upgrade => Key::NotifyUpgrade,
    };
    t(lang, key).to_string()
}

pub fn tf(lang: Lang, key: Key, subs: &[(&str, &str)]) -> String {
    let mut s = t(lang, key).to_string();
    for (name, value) in subs {
        // M14/F17: имя принимается и как `s`, и как `{s}` — шаблон
        // всегда хранит `{s}`. Раньше имя с фигурными скобками давал
        // поиск `{{s}}` → подстановка не срабатывала (баг таймера).
        let name = name.trim_matches(|c| c == '{' || c == '}');
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KEYS: &[Key] = &[
        Key::AppTitle,
        Key::Subtitle,
        Key::Exit,
        Key::Language,
        Key::Host,
        Key::Version,
        Key::ConfigPath,
        Key::DataPath,
        Key::M0Title,
        Key::M0Hint,
        Key::LoginTitle,
        Key::FieldLogin,
        Key::FieldPassword,
        Key::BtnLogin,
        Key::BtnLogout,
        Key::StatusConnecting,
        Key::StatusOnline,
        Key::StatusReconnecting,
        Key::StatusAuthExpired,
        Key::StatusStopped,
        Key::ErrFillFields,
        Key::RegisterTitle,
        Key::RegisterSubtitle,
        Key::FieldEmail,
        Key::FieldPassword2,
        Key::BtnRegister,
        Key::RegisterSending,
        Key::ErrRegisterFill,
        Key::ErrPasswordMismatch,
        Key::ErrBadEmail,
        Key::ErrRetryAfter,
        Key::CharactersTitle,
        Key::CharactersEmpty,
        Key::SlotsUsed,
        Key::SlotsFull,
        Key::BtnEnter,
        Key::BtnCreate,
        Key::BtnDelete,
        Key::BtnSwitchChar,
        Key::CreateTitle,
        Key::FieldCharName,
        Key::NameTooShort,
        Key::ProfileLabel,
        Key::ProfileWarrior,
        Key::ProfileArcher,
        Key::ProfileMage,
        Key::ProfileHint,
        Key::DeleteTitle,
        Key::DeleteConfirm,
        Key::DeleteTypeNick,
        Key::DeleteTimerLabel,
        Key::DeleteIn,
        Key::DeleteCancelBtn,
        Key::DeleteGameHint,
        Key::Loading,
        Key::GamePlaceholder,
        Key::LocationLabel,
        Key::HpLabel,
        Key::MpLabel,
        Key::XpPoolLabel,
        Key::ExitsLabel,
        Key::PlayersLabel,
        Key::MonstersLabel,
        Key::NpcsLabel,
        Key::CorpsesLabel,
        Key::HistoryLabel,
        Key::LogLabel,
        Key::Examine,
        Key::Attack,
        Key::EntityDetailTitle,
        Key::AltNavHint,
        Key::LootLabel,
        Key::NoExits,
        Key::CorpsePlayerHint,
        Key::SheetSkills,
        Key::SheetAttrs,
        // M17/F30d: основные характеристики (харизма убрана)
        Key::AttrStr,
        Key::AttrDex,
        Key::AttrVit,
        Key::AttrInt,
        Key::SheetInventory,
        Key::MaxLabel,
        Key::EquipLabel,
        Key::EmptySlot,
        Key::MenuUse,
        Key::MenuEquip,
        Key::MenuUnequip,
        Key::MenuDrop,
        Key::MenuDesc,
        Key::DeathTitle,
        Key::DeathLost,
        Key::DeathConfirm,
        Key::ItemDescTitle,
        Key::ReqLabel,
        Key::HotbarLabel,
        Key::NoCharge,
        Key::NotEnoughMp,
        Key::UpgradeTitle,
        Key::UpgradeBtn,
        Key::ChanceLabel,
        Key::CostLabel,
        Key::BtnSocketing,
        Key::SocketingTitle,
        Key::InsertRune,
        Key::ExtractRune,
        Key::NoRunes,
        Key::TargetLabel,
        Key::BtnFlee,
        Key::TabHint,
        Key::PowerStrike,
        Key::StatusesTab,
        Key::CrimeJournal,
        Key::ReloadJournal,
        Key::TotalLabel,
        Key::TakeLoot,
        Key::LootPlayerWarn,
        Key::NoStatuses,
        Key::JournalEmpty,
        Key::DurHm,
        Key::DurMs,
        Key::DurS,
        Key::QuestsTab,
        Key::ProfessionsTab,
        Key::TrackerLabel,
        Key::TrackBtn,
        Key::TrackedBtn,
        Key::RewardLabel,
        Key::BtnAccept,
        Key::BtnTurnIn,
        Key::BtnAbandon,
        Key::StateAvailable,
        Key::StateActive,
        Key::StateTurnIn,
        Key::StateDone,
        Key::RecipesLabel,
        Key::BtnCraft,
        Key::LearnLabel,
        Key::NoRecipes,
        Key::NoQuests,
        Key::MissingIngredients,
        Key::RankFmt,
        Key::ProfBlacksmith,
        Key::ProfAlchemist,
        Key::ProfScribe,
        Key::ProfWoodcarver,
        Key::ProfTailor,
        Key::ProfCook,
        Key::ProfHerbalist,
        Key::ProfHunter,
        Key::ChLabel,
        Key::ChSend,
        Key::ChReply,
        Key::ChFilter,
        Key::ChEmpty,
        Key::ChUnknownCmd,
        Key::ChSystem,
        Key::ChLocation,
        Key::ChGlobal,
        Key::ChParty,
        Key::ChGuild,
        Key::ChWhisper,
        Key::PartyLabel,
        Key::PartyCreateBtn,
        Key::PartyInviteBtn,
        Key::PartyKickBtn,
        Key::PartyLeaveBtn,
        Key::PartyLeader,
        Key::PartyInviteFrom,
        Key::PartyAcceptBtn,
        Key::PartyDeclineBtn,
        Key::PartyEmpty,
        Key::TradeLabel,
        Key::TradeYou,
        Key::TradeThem,
        Key::TradeGold,
        Key::TradeSetGold,
        Key::TradePropose,
        Key::TradeAccept,
        Key::TradeCancel,
        Key::TradeAddItem,
        Key::TradeValue,
        Key::TradeWarn,
        Key::TradeBtn,
        Key::GuildTab,
        Key::GuildNone,
        Key::GuildCreateLbl,
        Key::GuildCreateBtn,
        Key::GuildJoinBtn,
        Key::GuildLeaveBtn,
        Key::GuildInviteBtn,
        Key::GuildKickBtn,
        Key::GuildRankLbl,
        Key::GuildMembers,
        Key::GuildBrowseLbl,
        Key::GuildLogLbl,
        Key::SettingsTitle,
        Key::SettingsTheme,
        Key::ThemeDark,
        Key::ThemeLight,
        Key::SettingsFont,
        Key::SettingsChatLimit,
        Key::SettingsNotify,
        Key::SettingsHost,
        Key::SettingsApply,
        Key::SettingsWsWarn,
        Key::SettingsWsDenied,
        Key::SettingsHotkeys,
        Key::HkNavUp,
        Key::HkNavDown,
        Key::HkNavLeft,
        Key::HkNavRight,
        Key::HkCycleTarget,
        Key::HkPress,
        Key::SettingsBtn,
        Key::UpdateBadge,
        Key::On,
        Key::Off,
        Key::BtnClose,
        Key::NotifyDeath,
        Key::NotifyPartyInvite,
        Key::NotifyGuildInvite,
        Key::NotifyTradeOpen,
        Key::NotifyWhisper,
        Key::NotifyUpgrade,
        Key::BadgePvp,
        Key::BadgeSafe,
        Key::ExitCountdown,
        Key::ExitNowBtn,
        Key::CancelBtn,
        Key::SkillInfoBtn,
        Key::WarnCaps,
        Key::WarnRuLayout,
        Key::RolePlayer,
        Key::RoleAdmin,
        Key::RoleModerator,
        Key::SystemChatLocked,
        Key::ChDelete,
        Key::HkCleave,
        Key::HkRune,
        Key::BtnCharacter,
        Key::SheetParams,
        Key::SheetReputation,
        Key::HkOpenCharacter,
        Key::CharLevel,
        Key::CharXpTotal,
        Key::CharXpPool,
        Key::CharComingSoon,
        Key::ParAccuracy,
        Key::ParDamage,
        Key::ParCritChance,
        Key::ParCritDamage,
        Key::ParArmor,
        Key::ParDodge,
        Key::ParParry,
        Key::ParParryChance,
        Key::ParBlock,
        Key::ParBlockChance,
        Key::ParMagicDefense,
        Key::ParMagicDodge,
        Key::ParMagicResist,
        Key::ParMagicDispel,
        Key::ParMagicCritDamage,
        Key::ParMagicCritChance,
        Key::SkillGroupWeapon,
        Key::SkillGroupDefense,
        Key::SkillGroupMagic,
        Key::SkillGroupStealth,
        Key::SkillGroupPractice,
        Key::SkillGroupSocial,
        Key::HkConsumable,
        Key::HotbarNoTarget,
        Key::HotbarCooldown,
        Key::HotbarCdShort,
        Key::InventoryEmpty,
        Key::SettingsChatFont,
        Key::SettingsLogFont,
        Key::SettingsGeneral,
        Key::SettingsConnection,
        Key::SettingsChangeHost,
        Key::SettingsLogDamageColor,
        Key::SettingsLogAttackColor,
        Key::SettingsDamageBold,
        Key::SettingsFonts,
        // M16/F29g: вкладка «Все предметы» рюкзака (первая)
        Key::BpTabAll,
        // M16/F29f: вкладка «Броня» рюкзака
        Key::BpTabArmor,
        // M16/F29j: чекбокс «Запомнить пароль» на экране входа
        Key::RememberPassword,
        // M16/F29e: переводы характеристик, редкостей, слотов и типов
        Key::StatDamage,
        Key::StatHeal,
        Key::StatArmor,
        Key::StatMagic,
        Key::StatSpell,
        Key::StatSchool,
        Key::StatCharges,
        Key::StatMax,
        Key::SchoolFire,
        Key::SchoolFrost,
        Key::SchoolStorm,
        Key::RarityCommon,
        Key::RarityUncommon,
        Key::RarityRare,
        Key::RarityEpic,
        Key::RarityLegendary,
        Key::SlotHead,
        Key::SlotBody,
        Key::SlotMainHand,
        Key::SlotStaff,
        Key::SlotFeet,
        // M17/F30b: новые слоты и блокировка левой руки
        Key::SlotShoulders,
        Key::SlotBracers,
        Key::SlotGloves,
        Key::SlotLegs,
        Key::SlotCloak,
        Key::SlotRing1,
        Key::SlotRing2,
        Key::SlotAmulet,
        Key::SlotOffHand,
        Key::SlotArtifact1,
        Key::SlotArtifact2,
        Key::SlotBlocked,
        Key::ItemTypeWeapon,
        Key::ItemTypeStaff,
        Key::ItemTypePotion,
        Key::ItemTypeRune,
        Key::ItemTypeScroll,
        Key::ItemTypeArmor,
        Key::ItemTypeMoney,
        Key::ItemTypeHerb,
        Key::ItemTypeWoodOak,
        Key::ItemTypeParchment,
        Key::ItemTypeInk,
        Key::ItemTypeFlask,
        Key::ItemTypeChisel,
        Key::ItemTypeQuill,
        // M17/F30b: новые типы предметов
        Key::ItemTypeShield,
        Key::ItemTypeArtifact,
        // M16/F29d: группа «Атака» вкладки «Параметры»
        Key::ParGroupAttack,
        // M16/F29b: окно рюкзака и его хоткей
        Key::BackpackTitle,
        Key::BpTabWeapon,
        Key::BpTabPotion,
        Key::BpTabMagic,
        Key::BpTabMaterial,
        Key::BpTabTool,
        Key::BpTabOther,
        // M17/F30b: вкладка артефактов
        Key::BpTabArtifact,
        // M17/F30a: инкрустация руны + каст-бар
        Key::MenuInscribe,
        Key::InscribePick,
        Key::InscribeCasting,
        Key::InscribeNoWeapons,
        Key::BpCurrencies,
        Key::HkOpenBackpack,
    ];

    #[test]
    fn every_key_translated_on_every_lang() {
        for &key in ALL_KEYS {
            assert!(!t(Lang::Ru, key).is_empty(), "пустой RU: {key:?}");
            assert!(!t(Lang::En, key).is_empty(), "пустой EN: {key:?}");
        }
    }

    /// M14/F17: имя плейсхолдера допускается в скобках — вызовы
    /// `("{s}", …)` и `("s", …)` дают один результат.
    #[test]
    fn tf_accepts_braced_names() {
        let plain = tf(Lang::Ru, Key::ExitCountdown, &[("s", "7")]);
        let braced = tf(Lang::Ru, Key::ExitCountdown, &[("{s}", "7")]);
        assert_eq!(plain, braced, "обе формы имени эквивалентны");
        assert_eq!(braced, "Выход через 7 с", "подстановка: {braced}");
        assert!(!braced.contains('{'), "плейсхолдер заменён");
    }

    #[test]
    fn placeholders_consistent_between_langs() {
        // Плейсхолдеры должны совпадать в обоих языках — иначе format сломается.
        fn braces(s: &str) -> Vec<&str> {
            let mut found: Vec<&str> = s
                .split('{')
                .skip(1)
                .filter_map(|part| part.split('}').next())
                .collect();
            found.sort_unstable();
            found
        }
        for &key in ALL_KEYS {
            assert_eq!(
                braces(t(Lang::Ru, key)),
                braces(t(Lang::En, key)),
                "плейсхолдеры {key:?} расходятся"
            );
        }
    }

    #[test]
    fn tf_substitutes_placeholders() {
        let s = tf(Lang::Ru, Key::SlotsUsed, &[("used", "2"), ("limit", "8")]);
        assert_eq!(s, "2 из 8 слотов");
        let s = tf(Lang::Ru, Key::DeleteConfirm, &[("name", "Ким")]);
        assert!(
            s.contains("Ким") && !s.contains('{'),
            "плейсхолдер подставлен: {s}"
        );
    }

    #[test]
    fn lang_codes_round_trip() {
        assert_eq!(Lang::from_code("ru"), Lang::Ru);
        assert_eq!(Lang::from_code("en"), Lang::En);
        assert_eq!(Lang::from_code("de"), Lang::Ru, "неизвестный код → ru");
        assert_eq!(Lang::Ru.toggle(), Lang::En);
        assert_eq!(Lang::En.code(), "en");
    }
}

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

/// Запись реестра команд: wire-`type` и флаг идемпотентности (графа «Идемп.»
/// `docs/protocol.md` §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: &'static str,
    pub idempotent: bool,
}

macro_rules! impl_commands {
    ($(($name:ident, $ty:literal, $idem:literal)),* $(,)?) => {
        $(
            impl Command for $name {
                const TYPE: &'static str = $ty;
                const IDEMPOTENT: bool = $idem;
            }
        )*

        /// Все команды протокола в порядке объявления — по записи на строку
        /// `impl_commands!`. Реестр генерируется тем же списком, что и
        /// `impl Command`, поэтому «добавил команду и забыл зарегистрировать»
        /// невозможно. Источник истины для теста полноты `dispatch`
        /// (`ld2-server/tests/command_registry.rs`) и сверки с §7.
        pub const ALL_COMMANDS: &[CommandInfo] = &[
            $(
                CommandInfo { name: $ty, idempotent: $idem },
            )*
        ];
    };
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
    /// Стартовый профиль: Воин/Лучник/Маг (список — capability сервера).
    pub profile: String,
    /// M18/F34: пол (`male`/`female`); отсутствие/`null` → `male`
    /// (обратная совместимость, §11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CharSelect {
    pub char_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CharDelete {
    pub char_id: String,
}

/// M18/F44/ADR-009: повышение ранга навыка у тренера через диалог —
/// шкала навыка 100 % + очки опыта (пул) + плата из диалога NPC.
/// Заменяет `char.xp_invest` (обходил гейт 100 %).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SkillTrain {
    pub entity_id: String,
    pub skill_id: String,
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

/// M18/F35: диалог NPC — приветствие/строки с условиями + доступные квесты.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NpcTalk {
    pub entity_id: String,
}

/// M18/F37: ответ персонажа в диалоге — переход с `node_id` по
/// `reply_id` на следующую строку (без `next` у ответа — конец,
/// сервер отвечает `{node: null}`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NpcReply {
    pub entity_id: String,
    pub node_id: String,
    pub reply_id: String,
}

/// M18/F39: список товаров NPC — цены уже посчитаны сервером
/// (наценки/скидки), по валютным вкладкам.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ShopList {
    pub entity_id: String,
}

/// M18/F39: покупка у NPC: валюта (`def_id`) + товар (`def_id`),
/// одна партия по цене вкладки.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ShopBuy {
    pub entity_id: String,
    pub currency: String,
    pub def_id: String,
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

/// M21: повышение ранга профессии **у тренера** — уровень набран до
/// границы ранга, золото по прогрессии; ack несёт `{profession}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CraftRankUp {
    pub profession_id: String,
}

/// Окно профессий: сброс изученной профессии — прогресс теряется,
/// слот основной профессии освобождается; ack несёт `profession_id`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CraftReset {
    pub profession_id: String,
}

/// Снятие шкур: начать разделку трупа — сервер-джоб на5 с
/// (`action`: `skin` — снять шкуру, `gut` — освежевать); результат —
/// `evt/harvest/result`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CorpseHarvest {
    pub entity_id: String,
    pub action: String,
}

/// M20/F20b: добыча ресурса — взять 1 единицу из объекта-ресурса
/// (инструмент и ранг профессии проверяет сервер).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ResourceGather {
    pub entity_id: String,
}

/// M20: выбор действия в многостраничном игровом диалоге (заготовка).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DialogChoice {
    pub dialog_id: String,
    pub page_id: String,
    pub action_id: String,
}

/// M20: отправка ввода execute-диалога — набор значений полей
/// (id поля → значение); сервер возвращает ввод обработчику диалога.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DialogSubmit {
    pub dialog_id: String,
    #[serde(default)]
    pub values: serde_json::Map<String, serde_json::Value>,
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

/// M26: карта-граф мира (§7 аддитивно): чанк статического графа
/// локаций. `offset` — индекс первой локации чанка; ack
/// `{locations, offset, total}` — повторять до `offset >= total`
/// (MAX_MESSAGE_BYTES = 64 КБ — полный граф в одно сообщение
/// не влезает). Повтор безопасен (чтение).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MapWorld {
    #[serde(default)]
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Ping {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TimeSync {}

// ─── wire-типы (protocol.md §7: «Идемп.» = +) ────────────────────────────────

// ─── M24: админ-панель предметов (только роль admin, §9 E_PERMISSION) ───────

/// Каталог всех предметов для окна выдачи — один запрос при открытии
/// окна (вариант A): ack `{items:[{id,name,type,rarity,qty,upgrade,
/// gold_value}]}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AdminItems {}

/// Начать выдачу: сервер открывает execute-диалог количества
/// (`evt/dialog/open`, id `admin.give:<target>#<def_id>` → submit
/// stateless; разделитель `#`, т.к. цель и def могут содержать `:`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminGiveAsk {
    /// Цель: `self` | `char:<имя>` | `monster:<id>` | `corpse:<id>` |
    /// `container:<id>` | `shop:<npc_id>` (F24a — `self`).
    pub target: String,
    pub def_id: String,
}

/// Начать удаление: execute-диалог количества (§7), id
/// `admin.remove:<target>#<ref>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminRemoveAsk {
    pub target: String,
    /// Ссылка: uid в инвентаре / `loot:<corpse>:<idx>` / слот цели.
    #[serde(rename = "ref")]
    pub item_ref: String,
}

/// M24: «режим бога» — toggle неуязвимости (только admin); состояние
/// сессионное, живёт в мире (tick смотрит оттуда).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminGodMode {
    pub enabled: bool,
}

/// M24: удалить ВСЕ предметы выбранной цели (кнопка в админ-окне):
/// инвентарь — все записи, труп — все слоты, сундук — база (tombstone)
/// + надстройка, магазин — контент + надстройка, монстр — весь лут.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminClear {
    pub target: String,
}

/// M27 (hot reload контента): перечитать каталог `content/` на работающем
/// сервере — идемпотентный сид в БД при смене версии + замена диалогов/
/// магазинов/версии в памяти + сброс кэша карты мира. Только `role=admin`
/// → `E_PERMISSION`; ack `{version, applied, locations, items}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AdminContentReload {}

/// M24: содержимое выбранной цели правой колонки (кроме self —
/// ack `{items: [{ref, id, name, type, rarity, qty}]}` — рюкзак
/// (`self`/`char:<имя>`), лут трупа (`corpse:<id>`), будущий лут
/// монстра (`monster:<id>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminTargetInfo {
    pub target: String,
}

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
    (SkillTrain, "cmd/skill.train", false),
    (LocationLook, "cmd/location.look", true),
    (LocationMove, "cmd/location.move", false),
    (MapWorld, "cmd/map.world", true),
    (EntityInfo, "cmd/entity.info", true),
    (NpcTalk, "cmd/npc.talk", true),
    (NpcReply, "cmd/npc.reply", false),
    (ShopList, "cmd/shop.list", true),
    (ShopBuy, "cmd/shop.buy", false),
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
    (ResourceGather, "cmd/resource.gather", false),
    (CorpseHarvest, "cmd/corpse.harvest", false),
    (DialogChoice, "cmd/dialog.choice", false),
    (DialogSubmit, "cmd/dialog.submit", false),
    (AdminItems, "cmd/admin.items", true),
    (AdminGiveAsk, "cmd/admin.give.ask", false),
    (AdminRemoveAsk, "cmd/admin.remove.ask", false),
    (AdminGodMode, "cmd/admin.god_mode", false),
    (AdminTargetInfo, "cmd/admin.target.info", true),
    (AdminClear, "cmd/admin.clear", false),
    (AdminContentReload, "cmd/admin.content.reload", true),
    (UpgradePreview, "cmd/upgrade.preview", true),
    (UpgradeStart, "cmd/upgrade.start", false),
    (CraftList, "cmd/craft.list", true),
    (CraftRecipes, "cmd/craft.recipes", true),
    (CraftLearn, "cmd/craft.learn", false),
    (CraftReset, "cmd/craft.reset", false),
    (CraftRankUp, "cmd/craft.rank_up", false),
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
            MapWorld::TYPE,
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

    /// Команды из таблицы §7 `docs/protocol.md`: `(имя без `cmd/`, идемпотентна)`.
    fn protocol_doc_commands() -> Vec<(&'static str, bool)> {
        let doc = include_str!("../../../docs/protocol.md");
        let section = doc
            .split("## 7. Команды")
            .nth(1)
            .expect("§7 в docs/protocol.md")
            .split("\n## 8.")
            .next()
            .expect("конец §7");
        let mut out = Vec::new();
        for line in section.lines() {
            let line = line.trim();
            if !line.starts_with("| `") {
                continue;
            }
            let cells: Vec<&str> = line.split(" | ").collect();
            let idem = cells
                .get(3)
                .map(|c| c.trim_start().starts_with('+'))
                .expect("графа «Идемп.» в §7");
            let first = cells[0].trim_start_matches('|').trim();
            for (i, token) in first.split('`').enumerate() {
                if i % 2 == 0 {
                    continue; // текст между backtick-ами
                }
                let token = token.trim();
                let well_formed = !token.is_empty()
                    && token.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'
                    });
                if well_formed && !out.iter().any(|(name, _)| *name == token) {
                    out.push((token, idem));
                }
            }
        }
        out
    }

    #[test]
    fn registry_matches_protocol_doc() {
        let doc = protocol_doc_commands();
        assert!(!doc.is_empty(), "таблица §7 не распознана");
        let code: Vec<(&str, bool)> = ALL_COMMANDS
            .iter()
            .map(|c| {
                (
                    c.name.strip_prefix("cmd/").expect("префикс cmd/"),
                    c.idempotent,
                )
            })
            .collect();
        for (name, idem) in &code {
            let found = doc.iter().find(|(d, _)| d == name).unwrap_or_else(|| {
                panic!("`{name}` есть в ALL_COMMANDS, но не описана в docs/protocol.md §7")
            });
            assert_eq!(*idem, found.1, "идемпотентность `{name}` расходится с §7");
        }
        for (name, _) in &doc {
            assert!(
                code.iter().any(|(c, _)| c == name),
                "§7 описывает `{name}`, которой нет в ALL_COMMANDS"
            );
        }
        assert_eq!(code.len(), doc.len(), "число команд в реестре и §7");
    }

    #[test]
    fn registry_is_well_formed() {
        assert!(ALL_COMMANDS.len() >= 70, "реестр подозрительно короткий");
        let mut seen = std::collections::HashSet::new();
        for c in ALL_COMMANDS {
            assert!(c.name.starts_with("cmd/"), "{}", c.name);
            let rest = &c.name["cmd/".len()..];
            assert!(
                rest.chars().all(|ch| {
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '.'
                }),
                "{}",
                c.name
            );
            assert!(rest == "ping" || rest.contains('.'), "{}", c.name);
            assert!(seen.insert(c.name), "дубль {}", c.name);
        }
    }
}

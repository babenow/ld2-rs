//! Wire-имена протокола: `evt/*` и `cmd/*` в одном месте.
//!
//! Константы — алиасы существующих `Event::TYPE` / `Command::TYPE`
//! (реестры в [`crate::evt`]/[`crate::cmd`] остаются источником истины),
//! плюс префиксы доменов для диспетчеров (`starts_with`).

use super::cmd::Command;
use super::evt::Event;

/// Префикс всех событий и доменные префиксы (роутеры).
pub const EVT_PREFIX: &str = "evt/";
pub const EVT_ENTITY_PREFIX: &str = "evt/entity/";
pub const EVT_CHAR_PREFIX: &str = "evt/char/";
pub const EVT_MOB_PREFIX: &str = "evt/mob/";
pub const EVT_CORPSE_PREFIX: &str = "evt/corpse/";
pub const EVT_COMBAT_PREFIX: &str = "evt/combat/";
pub const EVT_ITEM_PREFIX: &str = "evt/item/";
pub const EVT_CHAT_PREFIX: &str = "evt/chat/";
pub const EVT_TRADE_PREFIX: &str = "evt/trade/";
pub const EVT_GUILD_PREFIX: &str = "evt/guild/";
pub const EVT_CRAFT_PREFIX: &str = "evt/craft/";
pub const EVT_QUEST_PREFIX: &str = "evt/quest/";
pub const EVT_CRIME_PREFIX: &str = "evt/crime/";
/// Префикс всех команд.
pub const CMD_PREFIX: &str = "cmd/";

// ─── события ────────────────────────────────────────────────────────────────
/// `evt/state/snapshot`
pub const EVT_STATE_SNAPSHOT: &str = <super::evt::StateSnapshot as Event>::TYPE;
/// `evt/state/delta`
pub const EVT_STATE_DELTA: &str = <super::evt::StateDelta as Event>::TYPE;
/// `evt/pong`
pub const EVT_PONG: &str = <super::evt::Pong as Event>::TYPE;
/// `evt/time.sync`
pub const EVT_TIME_SYNC: &str = <super::evt::TimeSync as Event>::TYPE;
/// `evt/error`
pub const EVT_ERROR: &str = <super::evt::ErrorNotice as Event>::TYPE;
/// `evt/char/xp`
pub const EVT_CHAR_XP: &str = <super::evt::CharXp as Event>::TYPE;
/// `evt/char/stats`
pub const EVT_CHAR_STATS: &str = <super::evt::CharStats as Event>::TYPE;
/// `evt/char/death`
pub const EVT_CHAR_DEATH: &str = <super::evt::CharDeath as Event>::TYPE;
/// `evt/location/changed`
pub const EVT_LOCATION_CHANGED: &str = <super::evt::LocationChanged as Event>::TYPE;
/// `evt/entity/spawn`
pub const EVT_ENTITY_SPAWN: &str = <super::evt::EntitySpawn as Event>::TYPE;
/// `evt/entity/move`
pub const EVT_ENTITY_MOVE: &str = <super::evt::EntityMove as Event>::TYPE;
/// `evt/entity/hp`
pub const EVT_ENTITY_HP: &str = <super::evt::EntityHp as Event>::TYPE;
/// `evt/entity/despawn`
pub const EVT_ENTITY_DESPAWN: &str = <super::evt::EntityDespawn as Event>::TYPE;
/// `evt/mob/attack`
pub const EVT_MOB_ATTACK: &str = <super::evt::MobAttack as Event>::TYPE;
/// `evt/mob/phase`
pub const EVT_MOB_PHASE: &str = <super::evt::MobPhase as Event>::TYPE;
/// `evt/mob/skill`
pub const EVT_MOB_SKILL: &str = <super::evt::MobSkill as Event>::TYPE;
/// `evt/mob/cast`
pub const EVT_MOB_CAST: &str = <super::evt::MobCast as Event>::TYPE;
/// `evt/char/afflict`
pub const EVT_CHAR_AFFLICT: &str = <super::evt::CharAfflict as Event>::TYPE;
/// `evt/corpse/created`
pub const EVT_CORPSE_CREATED: &str = <super::evt::CorpseCreated as Event>::TYPE;
/// `evt/corpse/looted`
pub const EVT_CORPSE_LOOTED: &str = <super::evt::CorpseLooted as Event>::TYPE;
/// `evt/combat/hit`
pub const EVT_COMBAT_HIT: &str = <super::evt::CombatHit as Event>::TYPE;
/// `evt/combat/cooldown`
pub const EVT_COMBAT_COOLDOWN: &str = <super::evt::CombatCooldown as Event>::TYPE;
/// `evt/combat/end`
pub const EVT_COMBAT_END: &str = <super::evt::CombatEnd as Event>::TYPE;
/// `evt/magic/cast`
pub const EVT_MAGIC_CAST: &str = <super::evt::MagicCast as Event>::TYPE;
/// `evt/item/added`
pub const EVT_ITEM_ADDED: &str = <super::evt::ItemAdded as Event>::TYPE;
/// `evt/item/removed`
pub const EVT_ITEM_REMOVED: &str = <super::evt::ItemRemoved as Event>::TYPE;
/// `evt/item/state`
pub const EVT_ITEM_STATE: &str = <super::evt::ItemState as Event>::TYPE;
/// `evt/item/dropped`
pub const EVT_ITEM_DROPPED: &str = <super::evt::ItemDropped as Event>::TYPE;
/// `evt/item/expired`
pub const EVT_ITEM_EXPIRED: &str = <super::evt::ItemExpired as Event>::TYPE;
/// `evt/resource/gathered`
pub const EVT_RESOURCE_GATHERED: &str = <super::evt::ResourceGathered as Event>::TYPE;

/// `evt/harvest/result`
pub const EVT_HARVEST_RESULT: &str = <super::evt::HarvestResult as Event>::TYPE;
/// `evt/char/level_up`
pub const EVT_CHAR_LEVEL_UP: &str = <super::evt::CharLevelUp as Event>::TYPE;
/// `evt/dialog/open`
pub const EVT_DIALOG_OPEN: &str = <super::evt::DialogOpen as Event>::TYPE;
/// `evt/upgrade/result`
pub const EVT_UPGRADE_RESULT: &str = <super::evt::UpgradeResult as Event>::TYPE;
/// `evt/craft/result`
pub const EVT_CRAFT_RESULT: &str = <super::evt::CraftResult as Event>::TYPE;
/// `evt/craft/profession`
pub const EVT_CRAFT_PROFESSION: &str = <super::evt::CraftProfession as Event>::TYPE;
/// `evt/quest/progress`
pub const EVT_QUEST_PROGRESS: &str = <super::evt::QuestProgress as Event>::TYPE;
/// `evt/quest/updated`
pub const EVT_QUEST_UPDATED: &str = <super::evt::QuestUpdated as Event>::TYPE;
/// `evt/char/buffs`
pub const EVT_CHAR_BUFFS: &str = <super::evt::CharBuffs as Event>::TYPE;
/// `evt/crime/status`
pub const EVT_CRIME_STATUS: &str = <super::evt::CrimeStatusUpdate as Event>::TYPE;
/// `evt/crime/added`
pub const EVT_CRIME_ADDED: &str = <super::evt::CrimeAdded as Event>::TYPE;
/// `evt/crime/removed`
pub const EVT_CRIME_REMOVED: &str = <super::evt::CrimeRemoved as Event>::TYPE;
/// `evt/chat/message`
pub const EVT_CHAT_MESSAGE: &str = <super::evt::ChatMessage as Event>::TYPE;
/// `evt/chat/deleted`
pub const EVT_CHAT_DELETED: &str = <super::evt::ChatDeleted as Event>::TYPE;
/// `evt/party/updated`
pub const EVT_PARTY_UPDATED: &str = <super::evt::PartyUpdated as Event>::TYPE;
/// `evt/trade/state`
pub const EVT_TRADE_STATE: &str = <super::evt::TradeState as Event>::TYPE;
/// `evt/trade/closed`
pub const EVT_TRADE_CLOSED: &str = <super::evt::TradeClosed as Event>::TYPE;
/// `evt/guild/updated`
pub const EVT_GUILD_UPDATED: &str = <super::evt::GuildUpdated as Event>::TYPE;
/// `evt/guild/log`
pub const EVT_GUILD_LOG: &str = <super::evt::GuildLog as Event>::TYPE;

/// `evt/session.start` — `SessionConfig` (ручной `impl Event`).
pub const EVT_SESSION_START: &str = <super::features::SessionConfig as Event>::TYPE;
/// Ad-hoc wire-события без отдельного типа.
pub const EVT_SESSION_REPLACED: &str = "evt/session/replaced";
pub const EVT_ACCOUNT_PREMIUM: &str = "evt/account/premium";
pub const EVT_SESSION_AFK: &str = "evt/session/afk";

// ─── команды ────────────────────────────────────────────────────────────────
/// `cmd/auth.login`
pub const CMD_AUTH_LOGIN: &str = <super::cmd::AuthLogin as Command>::TYPE;
/// `cmd/auth.register`
pub const CMD_AUTH_REGISTER: &str = <super::cmd::AuthRegister as Command>::TYPE;
/// `cmd/auth.resume`
pub const CMD_AUTH_RESUME: &str = <super::cmd::AuthResume as Command>::TYPE;
/// `cmd/auth.logout`
pub const CMD_AUTH_LOGOUT: &str = <super::cmd::AuthLogout as Command>::TYPE;
/// `cmd/session.refresh`
pub const CMD_SESSION_REFRESH: &str = <super::cmd::SessionRefresh as Command>::TYPE;
/// `cmd/char.list`
pub const CMD_CHAR_LIST: &str = <super::cmd::CharList as Command>::TYPE;
/// `cmd/char.create`
pub const CMD_CHAR_CREATE: &str = <super::cmd::CharCreate as Command>::TYPE;
/// `cmd/char.select`
pub const CMD_CHAR_SELECT: &str = <super::cmd::CharSelect as Command>::TYPE;
/// `cmd/char.delete`
pub const CMD_CHAR_DELETE: &str = <super::cmd::CharDelete as Command>::TYPE;
/// `cmd/skill.train`
pub const CMD_SKILL_TRAIN: &str = <super::cmd::SkillTrain as Command>::TYPE;
/// `cmd/location.look`
pub const CMD_LOCATION_LOOK: &str = <super::cmd::LocationLook as Command>::TYPE;
/// `cmd/location.move`
pub const CMD_LOCATION_MOVE: &str = <super::cmd::LocationMove as Command>::TYPE;
/// `cmd/map.world`
pub const CMD_MAP_WORLD: &str = <super::cmd::MapWorld as Command>::TYPE;
/// `cmd/entity.info`
pub const CMD_ENTITY_INFO: &str = <super::cmd::EntityInfo as Command>::TYPE;
/// `cmd/char.inspect`
pub const CMD_CHAR_INSPECT: &str = <super::cmd::CharInspect as Command>::TYPE;
/// `cmd/npc.talk`
pub const CMD_NPC_TALK: &str = <super::cmd::NpcTalk as Command>::TYPE;
/// `cmd/npc.reply`
pub const CMD_NPC_REPLY: &str = <super::cmd::NpcReply as Command>::TYPE;
/// `cmd/shop.list`
pub const CMD_SHOP_LIST: &str = <super::cmd::ShopList as Command>::TYPE;
/// `cmd/shop.buy`
pub const CMD_SHOP_BUY: &str = <super::cmd::ShopBuy as Command>::TYPE;
/// `cmd/combat.attack`
pub const CMD_COMBAT_ATTACK: &str = <super::cmd::CombatAttack as Command>::TYPE;
/// `cmd/combat.stop`
pub const CMD_COMBAT_STOP: &str = <super::cmd::CombatStop as Command>::TYPE;
/// `cmd/combat.flee`
pub const CMD_COMBAT_FLEE: &str = <super::cmd::CombatFlee as Command>::TYPE;
/// `cmd/skill.use`
pub const CMD_SKILL_USE: &str = <super::cmd::SkillUse as Command>::TYPE;
/// `cmd/item.use`
pub const CMD_ITEM_USE: &str = <super::cmd::ItemUse as Command>::TYPE;
/// `cmd/item.equip`
pub const CMD_ITEM_EQUIP: &str = <super::cmd::ItemEquip as Command>::TYPE;
/// `cmd/item.unequip`
pub const CMD_ITEM_UNEQUIP: &str = <super::cmd::ItemUnequip as Command>::TYPE;
/// `cmd/item.drop`
pub const CMD_ITEM_DROP: &str = <super::cmd::ItemDrop as Command>::TYPE;
/// `cmd/item.move`
pub const CMD_ITEM_MOVE: &str = <super::cmd::ItemMove as Command>::TYPE;
/// `cmd/magic.scroll_cast`
pub const CMD_MAGIC_SCROLL_CAST: &str = <super::cmd::MagicScrollCast as Command>::TYPE;
/// `cmd/magic.rune_cast`
pub const CMD_MAGIC_RUNE_CAST: &str = <super::cmd::MagicRuneCast as Command>::TYPE;
/// `cmd/socket.insert`
pub const CMD_SOCKET_INSERT: &str = <super::cmd::SocketInsert as Command>::TYPE;
/// `cmd/socket.extract`
pub const CMD_SOCKET_EXTRACT: &str = <super::cmd::SocketExtract as Command>::TYPE;
/// `cmd/resource.gather`
pub const CMD_RESOURCE_GATHER: &str = <super::cmd::ResourceGather as Command>::TYPE;
/// `cmd/corpse.harvest`
pub const CMD_CORPSE_HARVEST: &str = <super::cmd::CorpseHarvest as Command>::TYPE;
/// `cmd/dialog.choice`
pub const CMD_DIALOG_CHOICE: &str = <super::cmd::DialogChoice as Command>::TYPE;
/// `cmd/dialog.submit`
pub const CMD_DIALOG_SUBMIT: &str = <super::cmd::DialogSubmit as Command>::TYPE;
/// `cmd/admin.items`
pub const CMD_ADMIN_ITEMS: &str = <super::cmd::AdminItems as Command>::TYPE;
/// `cmd/admin.give.ask`
pub const CMD_ADMIN_GIVE_ASK: &str = <super::cmd::AdminGiveAsk as Command>::TYPE;
/// `cmd/admin.remove.ask`
pub const CMD_ADMIN_REMOVE_ASK: &str = <super::cmd::AdminRemoveAsk as Command>::TYPE;
/// `cmd/admin.god_mode`
pub const CMD_ADMIN_GOD_MODE: &str = <super::cmd::AdminGodMode as Command>::TYPE;
/// `cmd/admin.target.info`
pub const CMD_ADMIN_TARGET_INFO: &str = <super::cmd::AdminTargetInfo as Command>::TYPE;
/// `cmd/admin.clear`
pub const CMD_ADMIN_CLEAR: &str = <super::cmd::AdminClear as Command>::TYPE;
/// `cmd/admin.content.reload`
pub const CMD_ADMIN_CONTENT_RELOAD: &str = <super::cmd::AdminContentReload as Command>::TYPE;
/// `cmd/upgrade.preview`
pub const CMD_UPGRADE_PREVIEW: &str = <super::cmd::UpgradePreview as Command>::TYPE;
/// `cmd/upgrade.start`
pub const CMD_UPGRADE_START: &str = <super::cmd::UpgradeStart as Command>::TYPE;
/// `cmd/craft.list`
pub const CMD_CRAFT_LIST: &str = <super::cmd::CraftList as Command>::TYPE;
/// `cmd/craft.recipes`
pub const CMD_CRAFT_RECIPES: &str = <super::cmd::CraftRecipes as Command>::TYPE;
/// `cmd/craft.learn`
pub const CMD_CRAFT_LEARN: &str = <super::cmd::CraftLearn as Command>::TYPE;
/// `cmd/craft.reset`
pub const CMD_CRAFT_RESET: &str = <super::cmd::CraftReset as Command>::TYPE;
/// `cmd/craft.rank_up`
pub const CMD_CRAFT_RANK_UP: &str = <super::cmd::CraftRankUp as Command>::TYPE;
/// `cmd/craft.start`
pub const CMD_CRAFT_START: &str = <super::cmd::CraftStart as Command>::TYPE;
/// `cmd/quest.list`
pub const CMD_QUEST_LIST: &str = <super::cmd::QuestList as Command>::TYPE;
/// `cmd/quest.accept`
pub const CMD_QUEST_ACCEPT: &str = <super::cmd::QuestAccept as Command>::TYPE;
/// `cmd/quest.turn_in`
pub const CMD_QUEST_TURN_IN: &str = <super::cmd::QuestTurnIn as Command>::TYPE;
/// `cmd/quest.abandon`
pub const CMD_QUEST_ABANDON: &str = <super::cmd::QuestAbandon as Command>::TYPE;
/// `cmd/crime.log`
pub const CMD_CRIME_LOG: &str = <super::cmd::CrimeLog as Command>::TYPE;
/// `cmd/chat.send`
pub const CMD_CHAT_SEND: &str = <super::cmd::ChatSend as Command>::TYPE;
/// `cmd/chat.delete`
pub const CMD_CHAT_DELETE: &str = <super::cmd::ChatDelete as Command>::TYPE;
/// `cmd/party.create`
pub const CMD_PARTY_CREATE: &str = <super::cmd::PartyCreate as Command>::TYPE;
/// `cmd/party.invite`
pub const CMD_PARTY_INVITE: &str = <super::cmd::PartyInvite as Command>::TYPE;
/// `cmd/party.accept`
pub const CMD_PARTY_ACCEPT: &str = <super::cmd::PartyAccept as Command>::TYPE;
/// `cmd/party.decline`
pub const CMD_PARTY_DECLINE: &str = <super::cmd::PartyDecline as Command>::TYPE;
/// `cmd/party.leave`
pub const CMD_PARTY_LEAVE: &str = <super::cmd::PartyLeave as Command>::TYPE;
/// `cmd/party.kick`
pub const CMD_PARTY_KICK: &str = <super::cmd::PartyKick as Command>::TYPE;
/// `cmd/trade.open`
pub const CMD_TRADE_OPEN: &str = <super::cmd::TradeOpen as Command>::TYPE;
/// `cmd/trade.add_item`
pub const CMD_TRADE_ADD_ITEM: &str = <super::cmd::TradeAddItem as Command>::TYPE;
/// `cmd/trade.set_gold`
pub const CMD_TRADE_SET_GOLD: &str = <super::cmd::TradeSetGold as Command>::TYPE;
/// `cmd/trade.propose`
pub const CMD_TRADE_PROPOSE: &str = <super::cmd::TradePropose as Command>::TYPE;
/// `cmd/trade.accept`
pub const CMD_TRADE_ACCEPT: &str = <super::cmd::TradeAccept as Command>::TYPE;
/// `cmd/trade.cancel`
pub const CMD_TRADE_CANCEL: &str = <super::cmd::TradeCancel as Command>::TYPE;
/// `cmd/guild.browse`
pub const CMD_GUILD_BROWSE: &str = <super::cmd::GuildBrowse as Command>::TYPE;
/// `cmd/guild.create`
pub const CMD_GUILD_CREATE: &str = <super::cmd::GuildCreate as Command>::TYPE;
/// `cmd/guild.join`
pub const CMD_GUILD_JOIN: &str = <super::cmd::GuildJoin as Command>::TYPE;
/// `cmd/guild.leave`
pub const CMD_GUILD_LEAVE: &str = <super::cmd::GuildLeave as Command>::TYPE;
/// `cmd/guild.invite`
pub const CMD_GUILD_INVITE: &str = <super::cmd::GuildInvite as Command>::TYPE;
/// `cmd/guild.kick`
pub const CMD_GUILD_KICK: &str = <super::cmd::GuildKick as Command>::TYPE;
/// `cmd/guild.rank_set`
pub const CMD_GUILD_RANK_SET: &str = <super::cmd::GuildRankSet as Command>::TYPE;
/// `cmd/state.resync`
pub const CMD_STATE_RESYNC: &str = <super::cmd::StateResync as Command>::TYPE;
/// `cmd/ping`
pub const CMD_PING: &str = <super::cmd::Ping as Command>::TYPE;
/// `cmd/time.sync`
pub const CMD_TIME_SYNC: &str = <super::cmd::TimeSync as Command>::TYPE;

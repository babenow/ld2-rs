//! Доменные константы клиента (и общие с сервером значения).
//!
//! Слоты, типы предметов, редкости, профили, каналы, эффекты, школы и
//! профессии — вместо строковых литералов в UI/редьюсерах.

/// Слоты экипировки.
pub const SLOT_MAIN_HAND: &str = "main_hand";
pub const SLOT_OFF_HAND: &str = "off_hand";
pub const SLOT_HEAD: &str = "head";
pub const SLOT_BODY: &str = "body";
pub const SLOT_LEGS: &str = "legs";
pub const SLOT_FEET: &str = "feet";
pub const SLOT_GLOVES: &str = "gloves";
pub const SLOT_SHOULDERS: &str = "shoulders";
pub const SLOT_BRACERS: &str = "bracers";
pub const SLOT_CLOAK: &str = "cloak";
pub const SLOT_RING: &str = "ring";
pub const SLOT_RING1: &str = "ring1";
pub const SLOT_RING2: &str = "ring2";
pub const SLOT_AMULET: &str = "amulet";
pub const SLOT_ARTIFACT: &str = "artifact";
pub const SLOT_ARTIFACT1: &str = "artifact1";
pub const SLOT_ARTIFACT2: &str = "artifact2";
pub const SLOT_STAFF: &str = "staff";

/// Типы предметов (и близкие к ним id контента).
pub const ITEM_WEAPON: &str = "weapon";
pub const ITEM_ARMOR: &str = "armor";
pub const ITEM_SHIELD: &str = "shield";
pub const ITEM_GEM: &str = "gem";
pub const ITEM_RUNE: &str = "rune";
pub const ITEM_STAFF: &str = "staff";
pub const ITEM_POTION: &str = "potion";
pub const ITEM_SCROLL: &str = "scroll";
pub const ITEM_MONEY: &str = "money";
pub const ITEM_BAG: &str = "bag";
pub const ITEM_ARTIFACT: &str = "artifact";
pub const ITEM_HERB: &str = "herb";
pub const ITEM_WOOD_OAK: &str = "wood_oak";
pub const ITEM_PARCHMENT: &str = "parchment";
pub const ITEM_INK: &str = "ink";
pub const ITEM_FLASK: &str = "flask";
pub const ITEM_CHISEL: &str = "chisel";
pub const ITEM_QUILL: &str = "quill";
pub const ITEM_UPGRADE_STONE: &str = "upgrade_stone";
pub const ITEM_HIDE: &str = "hide";
pub const ITEM_HIDE_SCRAPS: &str = "hide_scraps";
pub const ITEM_MEAT: &str = "meat";
pub const ITEM_OFFAL: &str = "offal";
pub const ITEM_AMMO: &str = "ammo";
pub const ITEM_KEY: &str = "key";
pub const ITEM_AXE: &str = "axe";
pub const ITEM_PICKAXE: &str = "pickaxe";
pub const ITEM_SICKLE: &str = "sickle";
pub const ITEM_SKINNING_KNIFE: &str = "skinning_knife";
pub const ITEM_ORE_COPPER: &str = "ore_copper";
pub const ITEM_BRANCHES: &str = "branches";

/// Редкости предметов.
pub const RARITY_COMMON: &str = "common";
pub const RARITY_UNCOMMON: &str = "uncommon";
pub const RARITY_RARE: &str = "rare";
pub const RARITY_EPIC: &str = "epic";
pub const RARITY_LEGENDARY: &str = "legendary";

/// Профили персонажей.
pub const PROFILE_WARRIOR: &str = "warrior";
pub const PROFILE_ARCHER: &str = "archer";
pub const PROFILE_MAGE: &str = "mage";

/// Пол персонажа.
pub const GENDER_MALE: &str = "male";
pub const GENDER_FEMALE: &str = "female";

/// Каналы чата.
pub const CHANNEL_SYSTEM: &str = "system";
pub const CHANNEL_LOCATION: &str = "location";
pub const CHANNEL_GLOBAL: &str = "global";
pub const CHANNEL_PARTY: &str = "party";
pub const CHANNEL_GUILD: &str = "guild";
pub const CHANNEL_WHISPER: &str = "whisper";

/// Эффекты (`stats.effect.kind` / `CharAfflict.kind`).
pub const EFFECT_STUN: &str = "stun";
pub const EFFECT_SILENCE: &str = "silence";
pub const EFFECT_BLIND: &str = "blind";
pub const EFFECT_BLEED: &str = "bleed";
pub const EFFECT_ROOT: &str = "root";
pub const EFFECT_BURN: &str = "burn";

/// Школы магии.
pub const SCHOOL_FIRE: &str = "fire";
pub const SCHOOL_FROST: &str = "frost";
pub const SCHOOL_STORM: &str = "storm";

/// Профессии.
pub const PROFESSION_HERBALIST: &str = "herbalist";
pub const PROFESSION_ALCHEMIST: &str = "alchemist";
pub const PROFESSION_MINING: &str = "mining";
pub const PROFESSION_BLACKSMITH: &str = "blacksmith";
pub const PROFESSION_SKINNING: &str = "skinning";
pub const PROFESSION_LEATHERWORKING: &str = "leatherworking";
pub const PROFESSION_TAILOR: &str = "tailor";
pub const PROFESSION_SCRIBE: &str = "scribe";
pub const PROFESSION_WOODCUTTING: &str = "woodcutting";
pub const PROFESSION_WOODCARVER: &str = "woodcarver";
pub const PROFESSION_JEWELER: &str = "jeweler";
pub const PROFESSION_COOK: &str = "cook";
pub const PROFESSION_FISHING: &str = "fishing";
pub const PROFESSION_MEDIC: &str = "medic";

/// Направления выходов локации.
pub const DIR_NORTH: &str = "north";
pub const DIR_SOUTH: &str = "south";
pub const DIR_EAST: &str = "east";
pub const DIR_WEST: &str = "west";

/// Тема оформления.
pub const THEME_DARK: &str = "dark";

/// Тип сумки «общая».
pub const BAG_KIND_GENERAL: &str = "general";

/// Активный навык воина (панель действий).
pub const SKILL_POWER_STRIKE: &str = "power_strike";

/// Действия разделки трупа.
pub const ACTION_SKIN: &str = "skin";
pub const ACTION_GUT: &str = "gut";

/// Навык «Меч».
pub const SKILL_SWORD: &str = "sword";

/// Единицы времени.
pub const SECONDS_PER_MINUTE: i64 = 60;
pub const SECONDS_PER_HOUR: i64 = 3600;
pub const SECONDS_PER_DAY: i64 = 86_400;
pub const MS_PER_SECOND: i64 = 1000;

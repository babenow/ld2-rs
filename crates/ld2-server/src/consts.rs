//! Общие доменные константы сервера.
//!
//! Здесь — значения, которые повторяются в нескольких файлах и являются
//! частью домена (профили, эффекты, действия, единицы времени), а не
//! локальной формулой. Локальные числовые константы остаются в начале
//! своих файлов.

/// Профили персонажей (`CharacterState.profile`).
pub const PROFILE_WARRIOR: &str = "warrior";
pub const PROFILE_ARCHER: &str = "archer";
pub const PROFILE_MAGE: &str = "mage";

/// Пол персонажа (`CharacterState.gender`).
pub const GENDER_MALE: &str = "male";
pub const GENDER_FEMALE: &str = "female";

/// Эффекты приёмов/рун (`CharAfflict.kind`, `stats.effect.kind`).
pub const EFFECT_STUN: &str = "stun";
pub const EFFECT_SILENCE: &str = "silence";
pub const EFFECT_BLIND: &str = "blind";
pub const EFFECT_BLEED: &str = "bleed";
pub const EFFECT_ROOT: &str = "root";
pub const EFFECT_BURN: &str = "burn";

/// Действия разделки трупа (`corpse.harvest`).
pub const ACTION_SKIN: &str = "skin";
pub const ACTION_GUT: &str = "gut";

/// Навык без оружия (фолбэк `skills`).
pub const SKILL_UNARMED: &str = "unarmed";

/// Тип предмета «оружие».
pub const ITEM_TYPE_WEAPON: &str = "weapon";

/// Единицы времени протокола/тиков.
pub const MS_PER_SECOND: i64 = 1000;
pub const MS_PER_SECOND_F: f64 = 1000.0;

/// Типы предметов (`items.type`).
pub const ITEM_TYPE_ARMOR: &str = "armor";
pub const ITEM_TYPE_SHIELD: &str = "shield";
pub const ITEM_TYPE_GEM: &str = "gem";
pub const ITEM_TYPE_RUNE: &str = "rune";
pub const ITEM_TYPE_STAFF: &str = "staff";
pub const ITEM_TYPE_POTION: &str = "potion";
pub const ITEM_TYPE_SCROLL: &str = "scroll";
pub const ITEM_TYPE_MONEY: &str = "money";

/// Слоты экипировки (`items.slot` / `CharacterState.equip`).
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

/// Редкости (`items.rarity`).
pub const RARITY_COMMON: &str = "common";
pub const RARITY_UNCOMMON: &str = "uncommon";
pub const RARITY_RARE: &str = "rare";
pub const RARITY_EPIC: &str = "epic";
pub const RARITY_LEGENDARY: &str = "legendary";

/// Каналы чата (`evt/chat/message.channel`).
pub const CHANNEL_SYSTEM: &str = "system";
pub const CHANNEL_LOCATION: &str = "location";
pub const CHANNEL_GLOBAL: &str = "global";
pub const CHANNEL_PARTY: &str = "party";
pub const CHANNEL_GUILD: &str = "guild";
pub const CHANNEL_WHISPER: &str = "whisper";

/// Навыки, используемые в серверной логике.
pub const SKILL_SWORD: &str = "sword";
pub const SKILL_DAGGER: &str = "dagger";
pub const SKILL_AXE: &str = "axe";
pub const SKILL_POLEARM: &str = "polearm";
pub const SKILL_TWOHAND: &str = "twohand";
pub const SKILL_BOW: &str = "bow";
pub const SKILL_HAGGLE: &str = "haggle";

/// Профессия «Снятие шкур» и её инструмент.
pub const PROFESSION_SKINNING: &str = "skinning";
pub const ITEM_SKINNING_KNIFE: &str = "skinning_knife";

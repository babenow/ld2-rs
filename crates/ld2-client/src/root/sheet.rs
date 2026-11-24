//! Окно персонажа gpui (M15/F25; порт листа M10.4): левая колонка —
//! шкала опыта, экипировка, атрибуты, статусы со временем; правая —
//! вкладки (параметры/навыки/профессии/репутация/квесты/гильдия;
//! инвентарь — отдельное окно рюкзака, M16/F29b). Модалки предмета/
//! улучшения/инкрустации — прежние. Оболочка всплывающих окон
//! (scrim + drag/resize) — `Root::popup_window`.
//!
//! Правило дубликатов: атрибуты/статусы живут только в левой колонке —
//! одноимённых вкладок больше нет.
//!
//! Отличия от egui: инвентарь без drag&drop (действия в меню по ПКМ —
//! те же, что в контекстном меню egui), тултипы заменены модалкой
//! описания, combobox обучения — ряд кнопок. Состояние вкладки —
//! `Root::sheet_tab` (персистентно в config, ФР-19).

use ely::overlays::Dialog;
use gpui::{
    div, prelude::*, px, rgb, Animation, AnimationExt, AnyElement, Context, CursorStyle, Div,
    DragMoveEvent, EmptyView, MouseButton, SharedString, Window,
};
use ld2_core::l10n::{format_duration, t, tf, Key, Lang};
use ld2_core::store::{
    crime_kind_title, filter_active_buffs, filter_active_statuses, local_epoch, Act, Screen,
    UpgradeView,
};
use ld2_protocol::cmd::{
    CrimeLog, GuildBrowse, GuildLeave, GuildRankSet, ItemEquip, ItemUnequip, ItemUse,
    SocketExtract, SocketInsert, UpgradePreview,
};
use ld2_protocol::types::{CharacterState, ItemSnapshot, Severity, SkillSnapshot};

use crate::palette::Palette;
use crate::rune_glyph;
use ely::buttons::IconButton;
use ely::forms::{Choice, Input};
use ely::layout::ScrollArea;
use ely::menus::{ContextMenu, Menu, MenuItem};
use ely::navigation::Tabs;
use ely::primitives::{IconName, Tooltip, TooltipTrigger};

use super::quests::goals_summary;

mod character;
mod equip;
mod guild;
mod item_desc;
mod popup;
mod socket;
mod upgrade;
mod widgets;

use super::{
    act_btn, char_card_origin, clamp_char_size, scrollable_tab, DragSource, ItemDrag, PopupDrag,
    PopupKind, PopupResize, Root, SendSlot,
};
pub(crate) use item_desc::item_description_body;
pub(crate) use widgets::*;

/// Вкладки окна персонажа (M15/F25; персистентно строкой в config).
/// Порядок — как в правой колонке окна (спека пользователя).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetTab {
    /// Производные характеристики — наполняется в F26.
    Params,
    Skills,
    /// Репутация фракций — наполняется в F27.
    Reputation,
    // M18/F36: «Квесты» переехали в отдельное окно (правило дубликатов).
    Guild,
}

impl SheetTab {
    /// M16/F29b: «Инвентарь» убран из окна — он живёт в отдельном
    /// окне рюкзака (правило дубликатов).
    fn all() -> [(SheetTab, Key); 4] {
        [
            (SheetTab::Params, Key::SheetParams),
            (SheetTab::Skills, Key::SheetSkills),
            (SheetTab::Reputation, Key::SheetReputation),
            (SheetTab::Guild, Key::GuildTab),
        ]
    }

    /// M14/F12: иконка вкладки (Lucide-бандл Ely) — метка не меняется.
    pub(crate) fn icon(self) -> IconName {
        match self {
            SheetTab::Params => IconName::Gauge,
            SheetTab::Skills => IconName::Zap,
            SheetTab::Reputation => IconName::Shield,
            SheetTab::Guild => IconName::Flag,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SheetTab::Params => "params",
            SheetTab::Skills => "skills",
            SheetTab::Reputation => "reputation",
            SheetTab::Guild => "guild",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "skills" => SheetTab::Skills,
            "reputation" => SheetTab::Reputation,
            "guild" => SheetTab::Guild,
            // M15/F25: старые значения удалённых вкладок (attrs/statuses)
            // из сохранённого конфига → «Параметры» (ближайшая инфо-вкладка);
            // M16/F29b: «inventory» переехал в окно рюкзака;
            // M18/F36: «quests» переехал в отдельное окно;
            // M21: «professions» — окно профессий (как квесты).
            "attrs" | "statuses" | "inventory" | "quests" | "professions" => SheetTab::Params,
            _ => SheetTab::Params,
        }
    }
}

/// M15/F28: группы навыков в порядке GDD §3 (A–F); ид — значение
/// `skills.skill_group` из контента, ключ — заголовок группы.
const SKILL_GROUPS: [(&str, Key); 6] = [
    ("weapon", Key::SkillGroupWeapon),
    ("defense", Key::SkillGroupDefense),
    ("magic", Key::SkillGroupMagic),
    ("stealth", Key::SkillGroupStealth),
    ("practice", Key::SkillGroupPractice),
    ("social", Key::SkillGroupSocial),
];

// ── вспомогательные ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// M18/F58f: под инкрустацию выбирается слот, принимающий тип
    /// вставки — самоцвет не идёт в рунный слот (баг меню камня).
    #[test]
    fn first_compatible_socket_respects_accepts() {
        use ld2_protocol::types::{RuneInfo, Socket};
        let slot = |index: u32, accepts: Option<&str>, filled: bool| Socket {
            index,
            accepts: accepts.map(str::to_string),
            rune: filled.then(|| RuneInfo {
                class: None,
                name: "x".into(),
                spell: String::new(),
                charges: 1,
                max: 1,
                damage: None,
                def_id: None,
                glyph: None,
                detail: None,
                rarity: None,
            }),
        };
        // Оружие: rune, rune, gem — гем идёт в слот2, руна в свободный0.
        let weapon = [
            slot(0, Some("rune"), false),
            slot(1, Some("rune"), false),
            slot(2, Some("gem"), false),
        ];
        assert_eq!(first_compatible_socket(&weapon, "gem"), Some(2));
        assert_eq!(first_compatible_socket(&weapon, "rune"), Some(0));
        // Занятый слот пропускается.
        let weapon = [
            slot(0, Some("rune"), true),
            slot(1, Some("rune"), false),
            slot(2, Some("gem"), false),
        ];
        assert_eq!(first_compatible_socket(&weapon, "rune"), Some(1));
        // Старый сервер без accepts: первый свободный (§11).
        let legacy = [slot(0, None, false), slot(1, None, false)];
        assert_eq!(first_compatible_socket(&legacy, "gem"), Some(0));
        // Подходящего слота нет — None.
        let rune_only = [slot(0, Some("rune"), true)];
        assert_eq!(first_compatible_socket(&rune_only, "gem"), None);
        assert_eq!(first_compatible_socket(&[], "rune"), None);
    }

    /// M18/F58f: дубликаты рун — вставленная руна больше не
    /// предлагается для этого предмета; чужие имена не трогаются.
    #[test]
    fn rune_already_inserted_detects_duplicates() {
        use ld2_protocol::types::{RuneInfo, Socket};
        let socket_with = |rune_name: &str| Socket {
            index: 0,
            accepts: Some("rune".into()),
            rune: Some(RuneInfo {
                class: None,
                name: rune_name.into(),
                spell: "fire_dart".into(),
                charges: 7,
                max: 10,
                damage: None,
                def_id: None,
                glyph: Some("f".into()),
                detail: None,
                rarity: None,
            }),
        };
        let occupied = socket_with("Руна огня");
        let free = Socket {
            index: 1,
            accepts: Some("rune".into()),
            rune: None,
        };
        let sockets = [occupied, free];
        assert!(
            rune_already_inserted("Руна огня", sockets.iter()),
            "вставленная руна найдена"
        );
        assert!(
            !rune_already_inserted("Руна мороза", sockets.iter()),
            "другая руна не блокируется"
        );
        // Пустые слоты ничего не блокируют.
        let empty = [Socket {
            index: 1,
            accepts: Some("rune".into()),
            rune: None,
        }];
        assert!(!rune_already_inserted("Руна огня", empty.iter()));
    }

    /// M18/F58f: иконка слота — рунный слот даёт глиф руны (из поля
    /// glyph, фолбэк — хеш имени), слот самоцвета — ◈/○; у старого
    /// сервера без `accepts` — фолбэк по типу предмета (§11).
    #[test]
    fn slot_icon_distinguishes_runes_and_gems() {
        use ld2_protocol::types::{RuneInfo, Socket};
        let sock = |accepts: Option<&str>, filled: bool| Socket {
            index: 0,
            accepts: accepts.map(str::to_string),
            rune: filled.then(|| RuneInfo {
                class: None,
                name: "Руна огня".into(),
                spell: "fire_dart".into(),
                charges: 7,
                max: 10,
                damage: None,
                def_id: None,
                glyph: Some("f".into()),
                detail: None,
                rarity: None,
            }),
        };

        // Рунный слот: заполненный — глиф из контента (f), пустой — None.
        assert_eq!(
            slot_icon("weapon", &sock(Some("rune"), true)),
            SlotIcon::Rune(Some("f".into())),
            "заполненный рунный — глиф поля glyph"
        );
        assert_eq!(
            slot_icon("weapon", &sock(Some("rune"), false)),
            SlotIcon::Rune(None),
            "пустой рунный — отличается от пустого самоцвета"
        );
        // Слот самоцвета: пустой ○; занятый — раритет из wire (тут
        // None — фолбэк §11) либо знак раритета, если поле есть.
        assert_eq!(
            slot_icon("armor", &sock(Some("gem"), false)),
            SlotIcon::Gem {
                filled: false,
                rarity: None
            }
        );
        let with_rarity = Socket {
            index: 0,
            accepts: Some("gem".into()),
            rune: Some(RuneInfo {
                class: None,
                name: "Самоцвет силы".into(),
                spell: String::new(),
                charges: 0,
                max: 0,
                damage: None,
                def_id: None,
                glyph: None,
                detail: None,
                rarity: Some("rare".into()),
            }),
        };
        assert_eq!(
            slot_icon("armor", &with_rarity),
            SlotIcon::Gem {
                filled: true,
                rarity: Some("rare".into())
            },
            "занятый самоцвет несёт раритет для знака"
        );
        // Старый сервер без accepts: фолбэк по типу — броня = самоцвет,
        // оружие = руна.
        assert_eq!(
            slot_icon("armor", &sock(None, false)),
            SlotIcon::Gem {
                filled: false,
                rarity: None
            },
            "фолбэк: броня — слоты самоцветов"
        );
        assert_eq!(
            slot_icon("weapon", &sock(None, true)),
            SlotIcon::Rune(Some("f".into())),
            "фолбэк: оружие — рунные слоты"
        );
        // Неизвестное значение accepts — как отсутствующее (§11).
        assert_eq!(
            slot_icon("armor", &sock(Some("mystery"), false)),
            SlotIcon::Gem {
                filled: false,
                rarity: None
            }
        );
        // Занятый рунный слот — РОВНО глиф (не ◈, не квадрат):
        // пустой (□) и занятый (глиф руны) различимы.
        let occupied = slot_icon("weapon", &sock(Some("rune"), true));
        let empty = slot_icon("weapon", &sock(Some("rune"), false));
        assert_eq!(occupied, SlotIcon::Rune(Some("f".into())));
        assert_ne!(
            format!("{occupied:?}"),
            format!("{empty:?}"),
            "занят ≠ пуст"
        );
    }

    /// M18/F32: руна идёт только в оружие (weapon/staff), самоцвет —
    /// только в броню (armor); неизвестный тип вставки — ничего.
    #[test]
    fn inscribe_target_allowed_by_insert_type() {
        assert!(inscribe_target_allowed("rune", "weapon"));
        assert!(inscribe_target_allowed("rune", "staff"));
        assert!(!inscribe_target_allowed("rune", "armor"), "руна в броню");
        assert!(!inscribe_target_allowed("rune", "shield"));
        assert!(inscribe_target_allowed("gem", "armor"));
        assert!(
            !inscribe_target_allowed("gem", "weapon"),
            "самоцвет в оружие"
        );
        assert!(!inscribe_target_allowed("gem", "staff"));
        assert!(!inscribe_target_allowed("", "weapon"), "нет типа вставки");
        assert!(
            !inscribe_target_allowed("potion", "weapon"),
            "не инкрустация"
        );
    }

    /// M18/F58 (ADR-012): вставку решает `accepts` слота — отсюда
    /// самоцветы в оружие и отказ «руна в гемный слот»; у старого
    /// сервера (поля нет) — прежнее правило по типу предмета.
    #[test]
    fn inscribe_target_possible_by_socket_accepts() {
        use ld2_protocol::types::{ItemSnapshot, Socket};

        let item = |item_type: &str, sockets: Vec<Socket>| ItemSnapshot {
            slot: None,
            hands: None,
            def_id: None,
            uid: "i_t".into(),
            item_type: item_type.into(),
            name: "Тест".into(),
            rarity: "common".into(),
            upgrade: 0,
            stats: Default::default(),
            requires: None,
            sockets: Some(sockets),
            qty: Some(1),
            gold_value: None,
            bonuses: None,
            container: None,
            slot_index: None,
            bag_slots: None,
            extra: Default::default(),
        };
        let free = |accepts: Option<&str>, index: u32| Socket {
            index,
            rune: None,
            accepts: accepts.map(str::to_string),
        };
        let filled = |index: u32| Socket {
            index,
            rune: Some(ld2_protocol::types::RuneInfo {
                class: None,
                name: "Руна".into(),
                spell: "fire_dart".into(),
                charges: 1,
                max: 1,
                damage: None,
                def_id: None,
                glyph: None,
                detail: None,
                rarity: None,
            }),
            accepts: Some("rune".into()),
        };

        // Новый сервер: тип слота решает всё.
        let sword = item(
            "weapon",
            vec![free(Some("gem"), 0), free(Some("rune"), 1), filled(2)],
        );
        assert!(
            inscribe_target_possible("gem", &sword),
            "самоцвет в оружие — слот accepts:gem (F58)"
        );
        assert!(
            inscribe_target_possible("rune", &sword),
            "руна в рунный слот"
        );
        assert!(
            !inscribe_target_possible("potion", &sword),
            "не инкрустация"
        );

        // Занятый слот не предлагается.
        let only_filled = item("weapon", vec![filled(0)]);
        assert!(
            !inscribe_target_possible("rune", &only_filled),
            "слот занят"
        );

        // Гемный слот руне не подходит.
        let gem_only = item("weapon", vec![free(Some("gem"), 0)]);
        assert!(
            !inscribe_target_possible("rune", &gem_only),
            "руна в гемный слот"
        );

        // Старый сервер (без accepts) — прежнее правило по типу.
        let legacy_weapon = item("weapon", vec![free(None, 0)]);
        assert!(
            inscribe_target_possible("rune", &legacy_weapon),
            "фолбэк: руна в оружие"
        );
        assert!(
            !inscribe_target_possible("gem", &legacy_weapon),
            "фолбэк: самоцвет в оружие — как раньше"
        );
        let legacy_armor = item("armor", vec![free(None, 0)]);
        assert!(
            inscribe_target_possible("gem", &legacy_armor),
            "фолбэк: гем в броню"
        );
        assert!(
            !inscribe_target_possible("rune", &legacy_armor),
            "фолбэк: руна в броню"
        );
    }

    /// M16/F29e: лейблы переводят контент и падают на сырое значение
    /// для открытых строк (новый тип/слот/статс/школа).
    #[test]
    fn labels_translate_and_fall_back() {
        let ru = Lang::Ru;
        assert_eq!(rarity_label(ru, "rare"), "Редкий");
        assert_eq!(rarity_label(ru, "mythic"), "mythic", "fallback");
        assert_eq!(slot_label(ru, "main_hand"), "Правая рука");
        assert_eq!(slot_label(ru, "shoulders"), "Наплечники");
        assert_eq!(slot_label(ru, "artifact1"), "Артефакт 1");
        assert_eq!(slot_label(Lang::En, "off_hand"), "Off hand");
        assert_eq!(slot_label(ru, "tail"), "tail", "fallback");
        assert_eq!(type_label(ru, "money"), "Валюта");
        assert_eq!(type_label(ru, "shield"), "Щит");
        assert_eq!(type_label(ru, "artifact"), "Артефакт");
        assert_eq!(type_label(ru, "saddle"), "saddle", "fallback");
        assert_eq!(stat_label(ru, "dmg"), "Урон");
        assert_eq!(stat_label(ru, "damage"), "Урон", "dmg и damage — один ключ");
        assert_eq!(stat_label(ru, "piety"), "piety", "fallback");
        assert_eq!(school_label(ru, "fire"), "Огонь");
        assert_eq!(school_label(ru, "storm"), "Шторм");
        assert_eq!(school_label(ru, "necro"), "necro", "fallback");
    }

    /// M17/F30d: основные характеристики переведены; харизма (cha)
    /// убрана — неизвестный ключ падает на сырое значение.
    #[test]
    fn attr_labels_translate_and_fall_back() {
        let ru = Lang::Ru;
        let en = Lang::En;
        assert_eq!(attr_label(ru, "str"), "Сила");
        assert_eq!(attr_label(ru, "dex"), "Ловкость");
        assert_eq!(attr_label(ru, "vit"), "Стойкость");
        assert_eq!(attr_label(ru, "int"), "Интеллект");
        assert_eq!(attr_label(en, "str"), "Strength");
        assert_eq!(attr_label(en, "int"), "Intellect");
        assert_eq!(attr_label(ru, "cha"), "cha", "харизма убрана");
    }

    /// M15/F28: группы — в порядке GDD §3, иды уникальны.
    #[test]
    fn skill_groups_match_gdd_order() {
        let ids: Vec<&str> = SKILL_GROUPS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            ["weapon", "defense", "magic", "stealth", "practice", "social"],
            "порядок групп как в skills.md §3 (A–F)"
        );
        let unique: std::collections::HashSet<_> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "иды групп уникальны");
    }

    /// M15/F25: состав и порядок вкладок окна — по спеке пользователя
    /// (Параметры | Навыки | Репутация + нетронутые); M16/F29b:
    /// «Инвентарь» убран (окно рюкзака — отдельное); M21: «Профессии»
    /// убраны (отдельное окно, как квесты).
    #[test]
    fn character_window_tabs_cover_spec() {
        let tabs: Vec<&'static str> = SheetTab::all().iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            tabs,
            ["params", "skills", "reputation", "guild"],
            "порядок вкладок правой колонки"
        );
        for (tab, _) in SheetTab::all() {
            assert_eq!(SheetTab::parse(tab.as_str()), tab, "round-trip");
        }
        // Старые значения удалённых вкладок (правило дубликатов)
        // из сохранённого конфига маппятся на «Параметры».
        assert_eq!(SheetTab::parse("attrs"), SheetTab::Params);
        assert_eq!(SheetTab::parse("statuses"), SheetTab::Params);
        // M18/F36: вкладка «Квесты» переехала в отдельное окно.
        assert_eq!(SheetTab::parse("quests"), SheetTab::Params);
        assert_eq!(SheetTab::parse("inventory"), SheetTab::Params);
    }
}

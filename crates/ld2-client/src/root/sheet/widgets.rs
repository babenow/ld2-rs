//! Общие виджеты окна персонажа (RF3l).

use super::*;
use ld2_core::domain;

/// M18/F58f: как рисовать слот — **рунный знаком руны** (шрифт
/// Dwarven Runes), слот самоцвета — прежними ◈/○. Пустой рунный слот
/// отличим от пустого слота самоцвета (требование заказчика).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SlotIcon {
    /// Рунный слот: `Some(glyph)` — заполнен (глиф руны — латинские
    /// буквы, может быть несколько; заказчик 2026-11-20), `None` — пуст.
    Rune(Option<String>),
    /// Слот самоцвета: `filled` — заполнен; `rarity` — раритет
    /// вставленного (wire `RuneInfo.rarity`): есть → знак раритета
    /// (Symbola), нет (старый сервер/запись, §11) → прежний ◈;
    /// пустой слот → ○.
    Gem {
        filled: bool,
        rarity: Option<String>,
    },
}

/// M18/F58f: иконка слота. `accepts` из wire решает; у старого сервера
/// (поля нет) — фолбэк по типу предмета (как в инкрустации, §11):
/// броня — самоцветы, прочее — руны.
pub(crate) fn slot_icon(item_type: &str, socket: &ld2_protocol::types::Socket) -> SlotIcon {
    // Неизвестное/отсутствующее значение — фолбэк по типу (§11).
    let is_gem = match socket.accepts.as_deref() {
        Some(domain::ITEM_GEM) => true,
        Some(domain::ITEM_RUNE) => false,
        _ => item_type == domain::ITEM_ARMOR,
    };
    if is_gem {
        SlotIcon::Gem {
            filled: socket.rune.is_some(),
            rarity: socket.rune.as_ref().and_then(|r| r.rarity.clone()),
        }
    } else {
        SlotIcon::Rune(
            socket
                .rune
                .as_ref()
                .map(|r| crate::rune_glyph::resolve_glyph(r.glyph.as_deref(), &r.name)),
        )
    }
}

/// M18/F58f: ряд иконок слотов — **руна-слоты первыми** (требование
/// UI), далее слоты самоцветов; внутри группы — порядок `index`.
pub(crate) fn socket_icons_row(item_type: &str, sockets: &[ld2_protocol::types::Socket]) -> Div {
    let mut rune_slots: Vec<&ld2_protocol::types::Socket> = Vec::new();
    let mut gem_slots: Vec<&ld2_protocol::types::Socket> = Vec::new();
    for s in sockets {
        match s.accepts.as_deref() {
            Some(domain::ITEM_GEM) => gem_slots.push(s),
            // Рунный (и неизвестный/отсутствующий — фолбэк §11) —
            // старый сервер: сохраняем прежний порядок.
            _ => rune_slots.push(s),
        }
    }
    let mut row = div().flex().flex_row().gap_1().items_center();
    for s in rune_slots.into_iter().chain(gem_slots) {
        row = row.child(slot_icon_div(item_type, s));
    }
    row
}

/// M18/F58f: одна иконка слота (глиф руны шрифтом Dwarven Runes либо ◈/○).
pub(crate) fn slot_icon_div(item_type: &str, socket: &ld2_protocol::types::Socket) -> Div {
    match slot_icon(item_type, socket) {
        SlotIcon::Rune(Some(glyph)) => crate::rune_glyph::glyph_div(&glyph),
        SlotIcon::Rune(None) => crate::rune_glyph::empty_slot_glyph_div(),
        // Занятый самоцвет — знак его раритета (Symbola); у старого
        // сервера без поля rarity — прежний ◈ (§11).
        SlotIcon::Gem {
            filled: true,
            rarity: Some(r),
        } => crate::gem_glyph::gem_symbol_div(&r),
        SlotIcon::Gem {
            filled: true,
            rarity: None,
        } => div().child("◈"),
        SlotIcon::Gem { filled: false, .. } => div().child("○"),
    }
}

/// M18/F58f: бейджи строкой-рядом: +N, иконки слотов, ×N (паритет
/// egui; слоты нарисованы Div'ами — смешанные шрифты, §13).
pub(crate) fn badges_row(item: &ItemSnapshot) -> Div {
    let mut row = div().flex().flex_row().gap_1().items_center();
    if item.upgrade > 0 {
        row = row.child(div().child(format!("+{}", item.upgrade)));
    }
    if let Some(sockets) = &item.sockets {
        row = row.child(socket_icons_row(item.item_type.as_str(), sockets));
    }
    if let Some(qty) = item.qty.filter(|q| *q > 1) {
        row = row.child(div().child(format!("×{qty}")));
    }
    row
}

/// M18/F58f: подпись предмета в **рюкзаке** — строка1: название,
/// +N, ×N; строка2: иконки слотов (указание заказчика: слоты — на
/// новую строку). Слоты — Div'ами: руны их шрифтом.
pub(crate) fn item_label_stack(item: &ItemSnapshot, color: u32) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .gap_1()
        .items_center()
        .text_color(rgb(color));
    // M18/F58f: у самоцвета — знак раритета (Symbola) перед названием
    // (указание заказчика: рюкзак и описание).
    if item.item_type == domain::ITEM_GEM {
        row = row.child(crate::gem_glyph::gem_symbol_div(&item.rarity));
    }
    row = row.child(div().child(item.name.clone()));
    if item.upgrade > 0 {
        row = row.child(div().child(format!("+{}", item.upgrade)));
    }
    if let Some(qty) = item.qty.filter(|q| *q > 1) {
        row = row.child(div().child(format!("×{qty}")));
    }
    // Слоты — вплотную под названием (без промежутка), тем же
    // цветом, что и предмет (указание заказчика).
    //
    // M18/F58f (ADR-012 «делегаты списков»): вторая строка есть
    // **всегда** — у предметов без слотов невидимый заполнитель той же
    // высоты (□ как пустой слот, `invisible`): строки рюкзака одинаковой
    // высоты, а не «рваные» (одна строка vs две).
    let mut col = div().flex().flex_col().child(row);
    let second = match &item.sockets {
        Some(sockets) if !sockets.is_empty() => div()
            .text_color(rgb(color))
            .child(socket_icons_row(item.item_type.as_str(), sockets)),
        // Невидимый дубль высоты строки иконок (тот же символ, что
        // пустой руна-слот — идентичная линейная высота).
        _ => div().invisible().child("\u{25A1}"),
    };
    col = col.child(second);
    col
}

/// M18/F58f: подпись предмета в списках: название + бейджи (ряд Div'ов
/// — слоты рун рисуются их шрифтом).
pub(crate) fn item_label_row(item: &ItemSnapshot, color: u32) -> Div {
    div()
        .flex()
        .flex_row()
        .gap_1()
        .items_center()
        .text_color(rgb(color))
        .child(div().child(item.name.clone()))
        .child(badges_row(item))
}

/// Редкость → цвет (палитра §7.4).
/// MenuItem с действием над Root (запись через entity: замыкания `on_click`
/// должны быть `'static`, а `&mut Root` живёт только в цикле сборки).
pub(crate) fn menu_item(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    body: impl Fn(&mut Root, &mut Context<'_, Root>) + 'static,
) -> MenuItem {
    let this = cx.entity();
    MenuItem::new(label).on_click(move |_window, acx| {
        this.update(acx, |root, rcx| {
            body(root, rcx);
            rcx.notify();
        });
    })
}

/// Цвет редкости предмета (палитра §7.4).
/// Статичное внутреннее свечение рамки (inset) для эпика/легендарки —
/// в покое; анимация (переливание) запускается только по наведению
/// (заказчик 2026-11-20). Пустой vec — без свечения.
pub(crate) fn rarity_glow_inset(rarity: &str, pal: &Palette) -> Vec<gpui::BoxShadow> {
    if !matches!(rarity, domain::RARITY_EPIC | domain::RARITY_LEGENDARY) {
        return vec![];
    }
    let base = rarity_color(rarity, pal) & 0xff_ffff;
    vec![gpui::BoxShadow {
        color: gpui::rgba((base << 8) | 0x40).into(),
        offset: gpui::point(px(0.0), px(0.0)),
        blur_radius: px(8.0),
        spread_radius: px(0.0),
        inset: true,
    }]
}

pub(crate) fn rarity_color(rarity: &str, pal: &Palette) -> u32 {
    match rarity {
        domain::RARITY_COMMON => pal.muted,
        domain::RARITY_UNCOMMON => 0x98_c3_79,
        domain::RARITY_RARE => 0x61_af_ef,
        domain::RARITY_EPIC => 0xa0_52_2d,
        // Легендарка: яркий фиолетовый (орхидея) — тёмный
        // 0x6606b2 плохо читался на тёмной теме (заказчик).
        domain::RARITY_LEGENDARY => 0xc0_60_f0,
        _ => pal.text,
    }
}

/// Снятие шкур (M22): цвета **качества** шкур/обрезок — своя
/// палитра (**не** цвета редкости — указание заказчика); качество =
/// ранг профессии (`hide_q1..5` / `hide_scraps_q1..5`).
pub(crate) fn quality_color(def_id: Option<&str>) -> Option<u32> {
    let id = def_id?;
    let tier = id
        .strip_prefix("hide_q")
        .or_else(|| id.strip_prefix("hide_scraps_q"))?
        .parse::<u32>()
        .ok()?;
    Some(match tier {
        1 => 0x7a_6a_55,
        2 => 0x96_79_3f,
        3 => 0xb8_86_0b,
        4 => 0xda_a5_20,
        5 => 0xff_d7_00,
        _ => return None,
    })
}

/// Цвет названия предмета: качество (снятие шкур) иначе редкость.
pub(crate) fn item_color(item: &ItemSnapshot, pal: &Palette) -> u32 {
    quality_color(item.def_id.as_deref()).unwrap_or_else(|| rarity_color(&item.rarity, pal))
}

/// Тултип описания предмета (рюкзак и предметы на земле, R-dnd):
/// карточка с рамкой цвета редкости, описанием и пульсом наружу
/// (~2.5 с); задержка 1 с (заказчик 2026-11-20). Общий виджет.
#[allow(clippy::too_many_arguments)] // (id, child, detail, skills, theme, lang, rarity, line, key)
pub(crate) fn item_tooltip_trigger(
    id: impl Into<gpui::ElementId>,
    child: impl IntoElement,
    detail: serde_json::Value,
    skills: std::collections::HashMap<String, String>,
    theme: String,
    lang: Lang,
    rarity: String,
    name_line: String,
    anim_key: String,
) -> TooltipTrigger {
    let pulse_rarity = rarity.clone();
    let pulse_key = anim_key.clone();
    TooltipTrigger::new(id, child, move |_, _| {
        let pal = crate::palette::palette(&theme);
        let pulse_rarity = pulse_rarity.clone();
        let pulse_key = pulse_key.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .mx(px(-8.0))
            .my(px(-4.0))
            .p(px(crate::ui_tokens::SPACE_10))
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(rarity_color(&rarity, &pal)))
            .text_color(rgb(pal.text))
            .text_size(px(crate::ui_tokens::FONT_MD))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(name_line.clone()),
            )
            .child(item_description_body(lang, &pal, &detail, &skills))
            .with_animation(
                SharedString::from(format!("tip-pulse-{pulse_key}")),
                Animation::new(std::time::Duration::from_millis(2500)).repeat(),
                move |card, t| {
                    let alpha: f32 =
                        (0.30 + 0.20 * (2.0 * std::f32::consts::PI * t).sin()).clamp(0.10, 0.50);
                    let base = rarity_color(&pulse_rarity, &pal) & 0xff_ffff;
                    card.shadow(vec![gpui::BoxShadow {
                        color: gpui::rgba((base << 8) | (alpha * 255.0) as u32).into(),
                        offset: gpui::point(px(0.0), px(0.0)),
                        blur_radius: px(14.0),
                        spread_radius: px(2.0),
                        inset: false,
                    }])
                },
            )
            .into_any_element()
    })
    .delay(std::time::Duration::from_secs(1))
}

/// M16/F29e: название редкости (l10n; неизвестная — как есть).
pub(crate) fn rarity_label(lang: Lang, rarity: &str) -> String {
    match rarity {
        domain::RARITY_COMMON => t(lang, Key::RarityCommon),
        domain::RARITY_UNCOMMON => t(lang, Key::RarityUncommon),
        domain::RARITY_RARE => t(lang, Key::RarityRare),
        domain::RARITY_EPIC => t(lang, Key::RarityEpic),
        domain::RARITY_LEGENDARY => t(lang, Key::RarityLegendary),
        other => other,
    }
    .to_string()
}

/// M17/F30d: имя основной характеристики (l10n; харизма убрана,
/// неизвестный ключ — как есть).
pub(crate) fn attr_label(lang: Lang, key: &str) -> String {
    match key {
        "str" => t(lang, Key::AttrStr),
        "dex" => t(lang, Key::AttrDex),
        "vit" => t(lang, Key::AttrVit),
        "int" => t(lang, Key::AttrInt),
        "spir" => t(lang, Key::AttrSpir),
        other => other,
    }
    .to_string()
}

/// M31: описание атрибута для тултипа (character-params.md §1);
/// неизвестный ключ — без тултипа.
pub(crate) fn attr_tip(lang: Lang, key: &str) -> Option<String> {
    let tip = match key {
        "str" => Key::AttrStrTip,
        "dex" => Key::AttrDexTip,
        "vit" => Key::AttrVitTip,
        "int" => Key::AttrIntTip,
        "spir" => Key::AttrSpirTip,
        _ => return None,
    };
    Some(t(lang, tip).to_string())
}

/// M32: тултип-описание производного параметра по ключу его названия
/// (формулы — `character-params.md` §4).
pub(crate) fn param_tip(key: Key) -> Option<Key> {
    Some(match key {
        Key::ParAccuracy => Key::ParAccuracyTip,
        Key::ParDamage => Key::ParDamageTip,
        Key::ParCritChance => Key::ParCritChanceTip,
        Key::ParCritDamage => Key::ParCritDamageTip,
        Key::ParArmor => Key::ParArmorTip,
        Key::ParDodge => Key::ParDodgeTip,
        Key::ParParry => Key::ParParryTip,
        Key::ParParryChance => Key::ParParryChanceTip,
        Key::ParBlock => Key::ParBlockTip,
        Key::ParBlockChance => Key::ParBlockChanceTip,
        Key::ParMagicDefense => Key::ParMagicDefenseTip,
        Key::ParMagicDodge => Key::ParMagicDodgeTip,
        Key::ParMagicResist => Key::ParMagicResistTip,
        Key::ParMagicDispel => Key::ParMagicDispelTip,
        Key::ParMagicCritDamage => Key::ParMagicCritDamageTip,
        Key::ParMagicCritChance => Key::ParMagicCritChanceTip,
        _ => return None,
    })
}

/// M16/F29e: слот экипировки (конфиг открытый — fallback сырой).
pub(crate) fn slot_label(lang: Lang, slot: &str) -> String {
    match slot {
        domain::SLOT_HEAD => t(lang, Key::SlotHead),
        domain::SLOT_BODY => t(lang, Key::SlotBody),
        domain::SLOT_MAIN_HAND => t(lang, Key::SlotMainHand),
        domain::SLOT_STAFF => t(lang, Key::SlotStaff),
        domain::SLOT_FEET => t(lang, Key::SlotFeet),
        domain::SLOT_SHOULDERS => t(lang, Key::SlotShoulders),
        domain::SLOT_BRACERS => t(lang, Key::SlotBracers),
        domain::SLOT_GLOVES => t(lang, Key::SlotGloves),
        domain::SLOT_LEGS => t(lang, Key::SlotLegs),
        domain::SLOT_CLOAK => t(lang, Key::SlotCloak),
        domain::SLOT_RING1 => t(lang, Key::SlotRing1),
        domain::SLOT_RING2 => t(lang, Key::SlotRing2),
        domain::SLOT_AMULET => t(lang, Key::SlotAmulet),
        domain::SLOT_OFF_HAND => t(lang, Key::SlotOffHand),
        domain::SLOT_ARTIFACT1 => t(lang, Key::SlotArtifact1),
        domain::SLOT_ARTIFACT2 => t(lang, Key::SlotArtifact2),
        other => other,
    }
    .to_string()
}

/// M16/F29e: тип предмета (контент открытый — fallback сырой).
pub(crate) fn type_label(lang: Lang, item_type: &str) -> String {
    match item_type {
        other if other.starts_with("ore_") => t(lang, Key::ItemTypeOre),
        other if other.starts_with("raw_") => t(lang, Key::ItemTypeRawGem),
        domain::ITEM_WEAPON => t(lang, Key::ItemTypeWeapon),
        domain::ITEM_STAFF => t(lang, Key::ItemTypeStaff),
        domain::ITEM_POTION => t(lang, Key::ItemTypePotion),
        domain::ITEM_RUNE => t(lang, Key::ItemTypeRune),
        domain::ITEM_SCROLL => t(lang, Key::ItemTypeScroll),
        domain::ITEM_ARMOR => t(lang, Key::ItemTypeArmor),
        domain::ITEM_MONEY => t(lang, Key::ItemTypeMoney),
        domain::ITEM_HERB => t(lang, Key::ItemTypeHerb),
        domain::ITEM_WOOD_OAK => t(lang, Key::ItemTypeWoodOak),
        domain::ITEM_PARCHMENT => t(lang, Key::ItemTypeParchment),
        domain::ITEM_INK => t(lang, Key::ItemTypeInk),
        domain::ITEM_FLASK => t(lang, Key::ItemTypeFlask),
        domain::ITEM_CHISEL => t(lang, Key::ItemTypeChisel),
        domain::ITEM_QUILL => t(lang, Key::ItemTypeQuill),
        domain::ITEM_SHIELD => t(lang, Key::ItemTypeShield),
        domain::ITEM_ARTIFACT => t(lang, Key::ItemTypeArtifact),
        domain::ITEM_GEM => t(lang, Key::ItemTypeGem),
        // M18/F54: камни усиления (заточка).
        domain::ITEM_UPGRADE_STONE => t(lang, Key::ItemTypeStone),
        other => other,
    }
    .to_string()
}

/// M16/F29e: характеристика предмета (`stats`; `dmg` и `damage` —
/// один ключ). Значения школ переводятся отдельно.
pub(crate) fn stat_label(lang: Lang, stat: &str) -> String {
    match stat {
        "dmg" | "damage" => t(lang, Key::StatDamage),
        "heal" => t(lang, Key::StatHeal),
        "armor" => t(lang, Key::StatArmor),
        "magic" => t(lang, Key::StatMagic),
        "spell" => t(lang, Key::StatSpell),
        "school" => t(lang, Key::StatSchool),
        "charges" => t(lang, Key::StatCharges),
        "max" => t(lang, Key::StatMax),
        "attr" => t(lang, Key::StatAttr),
        "value" => t(lang, Key::StatValue),
        // M18/F54: качество камня усиления (процентные пункты).
        "quality" => t(lang, Key::StatQuality),
        // R-контент: метки статов руны.
        "cost" => t(lang, Key::StatCost),
        "class" => t(lang, Key::StatClass),
        other => other,
    }
    .to_string()
}

/// M18/F47: id производного параметра → название (бонусы предмета,
/// ADR-010); `damage` — общая метка «Урон».
pub(crate) fn param_label(lang: Lang, id: &str) -> String {
    let key = match id {
        "accuracy" => Key::ParAccuracy,
        "damage" | "damage_min" | "damage_max" => Key::ParDamage,
        "crit_chance" => Key::ParCritChance,
        "crit_damage" => Key::ParCritDamage,
        "armor" => Key::ParArmor,
        "dodge" => Key::ParDodge,
        "parry_min" | "parry_max" => Key::ParParry,
        "parry_chance" => Key::ParParryChance,
        "block_min" | "block_max" => Key::ParBlock,
        "block_chance" => Key::ParBlockChance,
        "magic_defense" => Key::ParMagicDefense,
        "magic_dodge" => Key::ParMagicDodge,
        "magic_resist" => Key::ParMagicResist,
        "magic_dispel" => Key::ParMagicDispel,
        "magic_crit_damage" => Key::ParMagicCritDamage,
        "magic_crit_chance" => Key::ParMagicCritChance,
        _ => return id.to_string(),
    };
    t(lang, key).to_string()
}

/// M18: производные параметры, которые измеряются в процентах.
pub(crate) fn param_is_percent(id: &str) -> bool {
    matches!(
        id,
        "accuracy"
            | "crit_chance"
            | "crit_damage"
            | "dodge"
            | "parry_chance"
            | "block_chance"
            | "magic_dodge"
            | "magic_resist"
            | "magic_dispel"
            | "magic_crit_damage"
            | "magic_crit_chance"
    )
}

/// M16/F29e: школа магии (значение `school`; fallback — сырое).
pub(crate) fn school_label(lang: Lang, school: &str) -> String {
    match school {
        domain::SCHOOL_FIRE => t(lang, Key::SchoolFire),
        domain::SCHOOL_FROST => t(lang, Key::SchoolFrost),
        domain::SCHOOL_STORM => t(lang, Key::SchoolStorm),
        other => other,
    }
    .to_string()
}

/// M17/F30b: слот для предмета — из поля `slot` снапшота (парные
/// ring/artifact занимают первую свободную ячейку); `None` — предмет
/// не экипируется либо пара занята целиком. Сервер — истина (§7).
pub(crate) fn slot_for(item: &ItemSnapshot, ch: &CharacterState) -> Option<String> {
    let slot = item.slot.as_deref()?;
    let free = |s: &str| ch.equip.get(s).map(|v| v.is_none()).unwrap_or(true);
    match slot {
        domain::SLOT_RING => [domain::SLOT_RING1, domain::SLOT_RING2]
            .into_iter()
            .find(|s| free(s))
            .map(str::to_string),
        domain::SLOT_ARTIFACT => [domain::SLOT_ARTIFACT1, domain::SLOT_ARTIFACT2]
            .into_iter()
            .find(|s| free(s))
            .map(str::to_string),
        other => Some(other.to_string()),
    }
}

/// Цвет severity (палитра §7.4).
pub(crate) fn severity_rgb(severity: Severity) -> u32 {
    match severity {
        Severity::High => 0xe0_6c_75,
        Severity::Medium => 0xd1_9a_66,
        Severity::Low => 0x98_c3_79,
        Severity::Unknown => 0xab_b2_bf,
    }
}

/// Полоса-прогресс (паритет egui ProgressBar).
pub(crate) fn bar(frac: f32, label: String, fill: u32, pal: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(140.0))
                .h(px(10.0))
                .rounded_md()
                .bg(rgb(pal.border))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac.clamp(0.0, 1.0)))
                        .rounded_md()
                        .bg(rgb(fill)),
                ),
        )
        .child(
            div()
                .text_size(px(crate::ui_tokens::FONT_SM))
                .text_color(rgb(pal.muted))
                .child(label),
        )
}

/// M15/F25: строка «подпись … значение» в панели опыта окна персонажа.
pub(crate) fn xp_kv_row(pal: &Palette, label: &str, value: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .text_size(px(crate::ui_tokens::FONT_SM))
        .child(div().text_color(rgb(pal.muted)).child(label.to_string()))
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(value.to_string()),
        )
}

/// M18/F32: типы целей инкрустации — руна идёт только в оружие
/// (`weapon`/`staff`), самоцвет (`gem`) — только в броню (`armor`).
/// Общая для окна «Куда инкрустировать…» и окна сокетов; сервер
/// M18/F58f (баг «инкрустация через меню камня»): первый свободный
/// слот, который **принимает** тип вставки по `accepts` (ADR-012:
/// самоцвет — только в gem-слот, руна — в рунный). У старого сервера
/// поля нет — прежнее поведение: первый свободный (§11).
pub(crate) fn first_compatible_socket(
    sockets: &[ld2_protocol::types::Socket],
    insert_type: &str,
) -> Option<usize> {
    sockets.iter().position(|s| {
        s.rune.is_none()
            && match s.accepts.as_deref() {
                Some(accepts) => accepts == insert_type,
                None => true,
            }
    })
}

/// M18/F58f: дубликаты рун не стакаются (указание заказчика) — есть
/// ли в сокетах предмета руна с таким именем (сервер откажет
/// `E_DUPLICATE_RUNE`, §9; фильтр списка — чтобы не предлагать).
pub(crate) fn rune_already_inserted<'a>(
    name: &str,
    sockets: impl IntoIterator<Item = &'a ld2_protocol::types::Socket>,
) -> bool {
    sockets
        .into_iter()
        .filter_map(|s| s.rune.as_ref())
        .any(|r| r.name == name)
}

/// проверяет то же правило (`socket.insert`, истина §7).
pub(crate) fn inscribe_target_allowed(insert_type: &str, target_type: &str) -> bool {
    match insert_type {
        domain::ITEM_GEM => target_type == domain::ITEM_ARMOR,
        domain::ITEM_RUNE => matches!(target_type, domain::ITEM_WEAPON | domain::ITEM_STAFF),
        _ => false,
    }
}

/// M18/F58 (ADR-012): можно ли вставить `insert_type` в предмет — есть
/// ли **свободный** слот, который его принимает (по `accepts` самого
/// слота). Отсюда самоцветы в оружие и отказ «руна в гемный слот».
///
/// У старого сервера поля `accepts` нет → прежнее правило по типу
/// предмета (`inscribe_target_allowed`), §11 — клиент фолбэчит.
pub(crate) fn inscribe_target_possible(insert_type: &str, item: &ItemSnapshot) -> bool {
    if !matches!(insert_type, domain::ITEM_RUNE | domain::ITEM_GEM) {
        return false;
    }
    item.sockets.iter().flatten().any(|socket| {
        if socket.rune.is_some() {
            return false; // слот занят
        }
        match socket.accepts.as_deref() {
            Some(accepts) => accepts == insert_type,
            // Старый сервер: фолбэк — прежнее правило по типу предмета.
            None => inscribe_target_allowed(insert_type, item.item_type.as_str()),
        }
    })
}

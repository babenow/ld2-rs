//! Окно персонажа: описание предмета (RF3l).

use super::*;
use ld2_core::domain;

/// Тело описания предмета — самостоятельный виджет: и окно описания,
/// и тултипы рюкзака/экипировки (заказчик 2026-11-20). Без `self`:
/// имена навыков-бонусов передаются готовыми (`skill_names`).
pub(crate) fn item_description_body(
    lang: Lang,
    pal: &Palette,
    detail: &serde_json::Value,
    skill_names: &std::collections::HashMap<String, String>,
) -> Div {
    let rarity = detail["rarity"].as_str().unwrap_or(domain::RARITY_COMMON);
    let item_type = detail["type"].as_str().unwrap_or("?");
    // M18/F57: справа запас под скроллбар (приём character_left_column)
    // — иначе индикатор прокрутки наезжал на жирные значения статов.
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .pr(px(crate::ui_tokens::SPACE_16));
    // M18/F58f: у рун — глиф предмета сразу под названием (шапка
    // окна), вверху тела (указание заказчика).
    if item_type == domain::ITEM_RUNE {
        let name = detail["name"].as_str().unwrap_or("rune");
        let glyph = detail["stats"].get("glyph").and_then(|v| v.as_str());
        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .child(crate::rune_glyph::glyph_div_for(glyph, name)),
        );
    } else if item_type == domain::ITEM_GEM {
        // M18/F58f: у самоцвета — знак раритета под названием
        // (указание заказчика: рюкзак и описание).
        col = col.child(div().flex().flex_row().items_baseline().child(
            crate::gem_glyph::gem_symbol_div(detail["rarity"].as_str().unwrap_or("")),
        ));
    }
    // M18/F58f: у рун уровень заточки в описании не нужен
    // (указание заказчика) — только «редкость · тип».
    let header = if item_type == domain::ITEM_RUNE {
        format!(
            "{} · {}",
            rarity_label(lang, rarity),
            type_label(lang, item_type)
        )
    } else {
        format!(
            "{} · {} · +{}",
            rarity_label(lang, rarity),
            type_label(lang, item_type),
            detail["upgrade"].as_i64().unwrap_or(0)
        )
    };
    col = col.child(
        div()
            .text_color(rgb(quality_color(detail["def_id"].as_str())
                .unwrap_or_else(|| rarity_color(rarity, pal))))
            .child(header),
    );
    // M18/F58f: слоты — вторая строка под «редкость · тип · +N»,
    // руна-слот первым; иконки различают руны и самоцветы.
    if let Ok(sockets) =
        serde_json::from_value::<Vec<ld2_protocol::types::Socket>>(detail["sockets"].clone())
    {
        if !sockets.is_empty() {
            // Цвет слотов — как у предмета (rarity-цвет строки).
            col = col.child(
                div()
                    .text_color(rgb(rarity_color(rarity, pal)))
                    .child(socket_icons_row(item_type, &sockets)),
            );
        }
    }
    col = item_stats_block(col, lang, pal, detail);
    col = item_extra_block(col, lang, pal, detail, skill_names);
    if let Some(value) = detail["gold_value"].as_i64() {
        col = col.child(
            div()
                .text_color(rgb(pal.muted))
                .child(format!("{value} зол.")),
        );
    }
    col
}

impl Root {
    // ── модалки (паритет egui-окон; вызываются из game_screen) ────────────

    /// Описание предмета (ФР-07).
    pub(crate) fn item_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        detail: serde_json::Value,
    ) -> Div {
        // Тот же виджет, что и тултип описания (заказчик 2026-11-20).
        let skills = self.skill_names_for(&detail);
        item_description_body(self.lang, pal, &detail, &skills).child(act_btn(
            cx,
            t(self.lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.item_detail = None;
                let _ = cx;
            },
        ))
    }

    /// M18/F53: описание предмета — карточка-попап (как окно
    /// персонажа): шапка с названием и крестиком, без затемнения.
    pub(crate) fn item_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let detail = self.store.item_detail.clone()?;
        let title = detail["name"].as_str().unwrap_or("?").to_string();
        // M18/F57: контент прокручивается — описание (статы, бонусы,
        // сокеты, цена) не помещалось в высоту карточки и обрезалось.
        //
        // Обёртка обязана быть **flex-колонкой**: ScrollArea рисует
        // ребёнка с `.size_full()` внутри `overflow_hidden`, поэтому его
        // собственная высота должна быть ограничена снаружи. Без `.flex().flex_col()`
        // его `flex_grow_1().min_h_0()` игнорируется (родитель не
        // flex-контейнер), высота равна контенту — скролла нет.
        // Приём сверен с `character_left_column` (рабочий).
        let body = div().flex().flex_col().flex_grow_1().min_h_0().child(
            ScrollArea::new("ld2-item-detail")
                .vertical()
                .flex_grow_1()
                .min_h_0()
                .child(self.item_view(cx, pal, detail)),
        );
        Some(self.popup_window(window, cx, pal, PopupKind::Item, title, None, body, None))
    }
}

/// Статы/параметры/эффекты предмета (RF3l: вынесено из
/// `item_description_body` для лимита 200 строк).
fn item_stats_block(
    mut col: gpui::Div,
    lang: Lang,
    pal: &Palette,
    detail: &serde_json::Value,
) -> gpui::Div {
    if let Some(stats) = detail["stats"].as_object() {
        // M18/F58f: строка-пара «label: value» в стиле статов окна.
        let kv = |label: String, value: String| {
            div()
                .flex()
                .flex_row()
                .justify_between()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(div().text_color(rgb(pal.muted)).child(label))
                .child(div().font_weight(gpui::FontWeight::BOLD).child(value))
        };
        // Заказчик 2026-11-20: у воина/лучника заклинание — «приём».
        let rune_class = stats
            .get("class")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        for (key, value) in stats {
            // M16/F29e: ключи и значения — на языке игрока
            // (школы — свои названия; прочие значения — как есть).
            // M18/F53: `param` — вложенный объект {id: значение};
            // показываем каждое производное как «Критический удар: +3%».
            if key == "param" {
                if let Some(params) = value.as_object() {
                    for (pid, pval) in params {
                        let raw = pval
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| pval.to_string());
                        let suffix = if param_is_percent(pid) { "%" } else { "" };
                        col = col.child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_between()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(pal.panel))
                                .child(
                                    div()
                                        .text_color(rgb(pal.muted))
                                        .child(param_label(lang, pid)),
                                )
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child(format!("{}{}", raw, suffix)),
                                ),
                        );
                    }
                    continue;
                }
            }
            // M18/F58f: описание рун (указание заказчика) — без
            // технического glyph, шанс — в процентах, бафф/эффект —
            // читаемые строки вместо JSON.
            if key == "glyph" {
                continue;
            }
            if key == "chance" {
                let pct = (value.as_f64().unwrap_or(0.0) * 100.0).round() as i64;
                col = col.child(kv(
                    t(lang, Key::RuneProcChance).to_string(),
                    format!("{pct}%"),
                ));
                continue;
            }
            if key.starts_with("buff") && value.is_object() {
                // Баффы рун (buff/buff2/…) — читаемые строки, не JSON:
                // «Урон: +10% · 12с · ×2» (pct — знак %).
                if let Some(b) = value.as_object() {
                    let param = b.get("param").and_then(|v| v.as_str()).unwrap_or("");
                    let val = b.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                    let pct = b.get("pct").and_then(|v| v.as_bool()).unwrap_or(false);
                    let dur = b.get("duration_s").and_then(|v| v.as_i64()).unwrap_or(0);
                    let stacks = b.get("max_stacks").and_then(|v| v.as_i64()).unwrap_or(1);
                    col = col.child(kv(
                        param_label(lang, param),
                        format!(
                            "+{val}{} · {} · ×{stacks}",
                            if pct { "%" } else { "" },
                            format_duration(lang, dur)
                        ),
                    ));
                }
                continue;
            }
            if key == "effect" {
                if let Some(e) = value.as_object() {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    let val = e.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                    let dur = format_duration(
                        lang,
                        e.get("duration_s").and_then(|v| v.as_i64()).unwrap_or(0),
                    );
                    let (label, val_s) = match kind {
                        domain::EFFECT_BLEED => (
                            t(lang, Key::EffectBleed).to_string(),
                            format!("{val}{} · {dur}", t(lang, Key::PerSecond)),
                        ),
                        // R-контент: «горение» — урон за тик с периодом.
                        domain::EFFECT_BURN => (
                            t(lang, Key::EffectBurn).to_string(),
                            format!("{val}{} · {dur}", t(lang, Key::PerTick),),
                        ),
                        domain::EFFECT_STUN => (t(lang, Key::EffectStun).to_string(), dur.clone()),
                        domain::EFFECT_ROOT => (t(lang, Key::EffectRoot).to_string(), dur.clone()),
                        other => (other.to_string(), dur),
                    };
                    col = col.child(kv(label, val_s));
                }
                continue;
            }
            // Заказчик 2026-11-20: неразобранный JSON в описании
            // не показываем — незнакомые структурные поля контента
            // (объекты/массивы) пропускаем, а не сваливаем строкой.
            if value.is_object() || value.is_array() {
                continue;
            }
            let raw = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            let value = if key == "school" {
                school_label(lang, &raw)
            } else if key == "class" {
                // Класс руны: warrior/mage/archer → метка языка.
                match raw.as_str() {
                    domain::PROFILE_WARRIOR => t(lang, Key::ProfileWarrior).to_string(),
                    domain::PROFILE_MAGE => t(lang, Key::ProfileMage).to_string(),
                    domain::PROFILE_ARCHER => t(lang, Key::ProfileArcher).to_string(),
                    _ => raw.clone(),
                }
            } else if key == "attr" {
                // M17/F30c: значение самоцвета — «str» → «Сила».
                attr_label(lang, &raw)
            } else if key == "spell" {
                // M17/F30f: id заклинания → название (без storm_bolt).
                ld2_core::spells::spell_key(&raw)
                    .map(|k| t(lang, k).to_string())
                    .unwrap_or(raw)
            } else {
                raw
            };
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(div().text_color(rgb(pal.muted)).child({
                        if key == "spell"
                            && matches!(
                                rune_class.as_str(),
                                domain::PROFILE_WARRIOR | domain::PROFILE_ARCHER
                            )
                        {
                            // У воина/лучника это приём, не заклинание.
                            t(lang, Key::SpellTechnique).to_string()
                        } else {
                            stat_label(lang, key)
                        }
                    }))
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(value)),
            );
        }
    }
    // M18/F47/ADR-010: бонусы предмета — «Сила +3», «Точность +4%»,
    // «Владение мечом +2» (контент + инстанс, слитые сервером).
    col
}

/// Бонусы, сокеты и цена предмета (RF3l: вынесено из
/// `item_description_body` для лимита 200 строк).
fn item_extra_block(
    mut col: gpui::Div,
    lang: Lang,
    pal: &Palette,
    detail: &serde_json::Value,
    skill_names: &std::collections::HashMap<String, String>,
) -> gpui::Div {
    if let Some(bonuses) = detail["bonuses"].as_array() {
        if !bonuses.is_empty() {
            col = col.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_XS))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::ItemBonuses)),
            );
            for bonus in bonuses {
                let kind = bonus["kind"].as_str().unwrap_or_default();
                let id = bonus["id"].as_str().unwrap_or_default();
                let value = bonus["value"].as_i64().unwrap_or(0);
                let pct = bonus["pct"].as_bool().unwrap_or(false);
                let name = match kind {
                    "attr" => attr_label(lang, id),
                    "param" => param_label(lang, id),
                    "skill" => skill_names
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| id.to_string()),
                    _ => id.to_string(),
                };
                let sign = if value >= 0 { "+" } else { "" };
                // M18/F56: та же плитка, что у статов выше —
                // label приглушённым слева, значение жирным вправо
                // (иначе бонусы «плыли» относительно статов).
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .child(div().text_color(rgb(pal.muted)).child(name))
                        .child(div().font_weight(gpui::FontWeight::BOLD).child(format!(
                            "{}{}{}",
                            sign,
                            value,
                            if pct { "%" } else { "" }
                        ))),
                );
            }
        }
    }
    if let Some(sockets) = detail["sockets"].as_array() {
        for socket in sockets {
            // M18/F58f: знаки рунных слотов — глифы шрифта Dwarven
            // Runes (rune_glyph); слоты самоцветов — прежние ◈/○.
            let is_rune_slot = socket["accepts"].as_str() != Some(domain::ITEM_GEM);
            if let Some(rune) = socket["rune"].as_object() {
                // M17/F30c: самоцвет — без зарядов (заряды у рун).
                // M18/F58f: знак — контент-поле `glyph`, фолбэк — хеш.
                // Доступ к Map — только через `.get()`: индексация
                // отсутствующего ключа у serde_json::Map паникует
                // («no entry found for key» — вылет при открытии
                // описания).
                let name = rune.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let glyph =
                    rune_glyph::glyph_div_for(rune.get("glyph").and_then(|v| v.as_str()), name);
                let label = if is_rune_slot {
                    if rune
                        .get("spell")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s.is_empty())
                    {
                        name.to_string()
                    } else {
                        format!(
                            "{}: {}/{}",
                            name,
                            rune.get("charges").and_then(|v| v.as_i64()).unwrap_or(0),
                            rune.get("max").and_then(|v| v.as_i64()).unwrap_or(0)
                        )
                    }
                } else {
                    name.to_string()
                };
                // M18/F58f: под занятым слотом — подробности, что
                // даёт вставка (урон/шанс/бафф/эффект/трата; сервер
                // формирует строки, §8).
                let mut cell = div().flex().flex_col().child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .items_baseline()
                        .child(if is_rune_slot {
                            glyph
                        } else {
                            // M18/F58f (правка заказчика): занятый
                            // самоцвет — знак его раритета (Symbola);
                            // старый сервер без rarity — прежний ◈
                            // (§11).
                            match rune.get("rarity").and_then(|v| v.as_str()) {
                                Some(rarity) => crate::gem_glyph::gem_symbol_div(rarity),
                                None => div().child("◈"),
                            }
                        })
                        .child(div().child(label)),
                );
                // `.get`, а не `rune["detail"]`: у serde_json::Map
                // индексация отсутствующего ключа паникует (в
                // отличие от Value → Null) — вылет при открытии
                // описания рун без подписи (M18/F58f).
                if let Some(details) = rune.get("detail").and_then(|d| d.as_array()) {
                    for line in details.iter().filter_map(|d| d.as_str()) {
                        cell = cell.child(
                            div()
                                .pl(px(crate::ui_tokens::SPACE_20))
                                .text_color(rgb(pal.muted))
                                .child(line.to_string()),
                        );
                    }
                }
                col = col.child(cell);
            } else if is_rune_slot {
                let glyph = rune_glyph::empty_slot_glyph_div();
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .items_baseline()
                        .child(glyph)
                        .child(div().text_color(rgb(pal.muted)).child("—")),
                );
            } else {
                col = col.child(div().text_color(rgb(pal.muted)).child("○ —"));
            }
        }
    }
    col
}

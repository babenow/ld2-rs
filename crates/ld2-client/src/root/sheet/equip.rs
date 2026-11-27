//! Окно персонажа: equip (RF3l).

use ld2_core::domain;

use super::*;

impl Root {
    // ── вкладка «Инвентарь» (ФР-07) ───────────────────────────────────────

    /// M15/F25: экипировка — верх левой колонки (перенос из «Инвентаря»
    /// по правилу дубликатов): слоты с действиями снятия/описания.
    pub(crate) fn equip_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let equip_slots: Vec<String> = self
            .store
            .session
            .as_ref()
            .map(|s| s.inventory.equip_slots.clone())
            .unwrap_or_default();

        let mut col = div().flex().flex_col().gap_2();

        // Слоты экипировки (ФР-07): вместо drag&drop — действия в строке.
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::EquipLabel)),
        );
        // M18 (фича): строки собираем — ниже раскладываем в две
        // колонки одинаковой высоты; слоты рук — отдельным блоком
        // внизу (указание заказчика).
        // R-dnd: тянется ли предмет (для подсветки зон).
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut hand_rows: Vec<AnyElement> = Vec::new();
        for slot in &equip_slots {
            let row = self.equip_row(slot, &snap, lang, cx, pal);
            if matches!(
                slot.as_str(),
                domain::SLOT_MAIN_HAND | domain::SLOT_OFF_HAND
            ) {
                hand_rows.push(row.into_any_element());
            } else {
                rows.push(row.into_any_element());
            }
        }

        // Две колонки одинаковой высоты: ряд растягивает обе до
        // самой длинной (flex stretch), ширины равны (flex_grow).
        let two_cols = |items: Vec<AnyElement>| -> Div {
            let half = items.len().div_ceil(2);
            // flex_1 (basis 0) — колонки строго по 50 %: длинное
            // название не расширяет свою ячейку (заказчик 2026-11-27).
            let mut left = div().flex().flex_col().gap_1().min_w_0().flex_1();
            let mut right = div().flex().flex_col().gap_1().min_w_0().flex_1();
            for (i, row) in items.into_iter().enumerate() {
                if i < half {
                    left = left.child(row);
                } else {
                    right = right.child(row);
                }
            }
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_stretch()
                .child(left)
                .child(right)
        };
        col = col.child(two_cols(rows));

        // M18 (указание заказчика): слоты рук — в самый низ, ниже всей
        // экипировки, отделены чертой.
        if !hand_rows.is_empty() {
            col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
            col = col.child(two_cols(hand_rows));
        }

        col
    }

    /// Строка слота экипировки: карточка с названием слота, предметом,
    /// сокетами и меню действий (RF3l: вынесено из `equip_block`, чтобы
    /// функция уложилась в лимит 200 строк).
    pub(crate) fn equip_row(
        &mut self,
        slot: &String,
        snap: &ld2_protocol::evt::StateSnapshot,
        lang: ld2_core::l10n::Lang,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> gpui::AnyElement {
        let (mut row, item) = self.equip_card(slot, snap, lang, cx, pal);
        let item = item.as_ref();
        let Some(item) = item else {
            // M18 (указание заказчика): пустые слоты — **не
            // подсвечиваются** и выглядят disabled (приглушены);
            // «Заблокировано двуручным оружием» больше нет — слот
            // показывается как обычный пустой (метка слота сверху
            // повторяет слот оружия).
            row = row.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::EmptySlot)),
            );
            row = row.opacity(0.55);
            return row.into_any_element();
        };
        // R-dnd: тянем предмет ИЗ слота (выброс/снятие).
        let drag_payload = ItemDrag {
            uid: item.uid.clone(),
            from: DragSource::Equip(slot.clone()),
            slot_req: item.slot.clone(),
        };
        let drag_this = cx.entity();
        let ghost_name = item.name.clone();
        let ghost_color = rarity_color(&item.rarity, pal);
        let ghost_theme = self.config.theme.clone();
        row = row.on_drag(drag_payload, move |drag, _, _, cx| {
            tracing::info!("dnd: старт drag {} из экипировки", drag.uid);
            drag_this.update(cx, |root, cx| {
                root.drag_item = Some(drag.clone());
                root.popup_scrim_down = false;
                cx.notify();
            });
            // Карточка предмета под курсором (заказчик).
            let name = ghost_name.clone();
            let color = ghost_color;
            let theme = ghost_theme.clone();
            cx.new(move |_| crate::root::ItemDragGhost { name, color, theme })
        });
        row = self.equip_card_title(row, item, lang, pal);
        // Заказчик 2026-11-20: описание предмета — в тултипе (тот же
        // виджет, что и окно описания; появляется плавно).
        let tip_detail = serde_json::to_value(item).unwrap_or_default();
        let tip_skills = self.skill_names_for(&tip_detail);
        let tip_theme = self.config.theme.clone();
        let tip_name = item.name.clone();
        let tip_lang = lang;
        let tip_rarity = item.rarity.clone();
        // Меню предмета (F29d): Описание и Снять — вместо кнопок.
        let menu = self.equip_card_menu(cx, lang, slot, item);
        // Запрос меню: левый и правый клик по строке (номер растёт —
        // Ely переоткрывает; в manual-режиме host-правый клик выключен).
        let request = match &self.equip_menu {
            Some((s, n, at)) if s == slot => Some((*n, *at)),
            _ => None,
        };
        let slot_right = slot.clone();
        // R-dnd: захват предмета — НЕ клик по scrim (иначе
        // неудачный захват закрывал окно рюкзака под ним).
        let row = row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |root, _, _, _| {
                root.popup_scrim_down = false;
            }),
        );
        // Контекстное меню — только ПКМ (заказчик: ЛКМ-меню убрано).
        let row = row.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                window.prevent_default();
                this.bump_equip_menu(slot_right.clone(), event.position, cx);
            }),
        );
        // Переливание (заказчик 2026-11-20): эпик/легендарка — на
        // наведении играет ЯРКОСТЬ цвета редкости (бриллиант);
        // без наведения — статичное внутреннее свечение.
        let hover_key = format!("eq:{slot}");
        let shimmering = matches!(
            item.rarity.as_str(),
            domain::RARITY_EPIC | domain::RARITY_LEGENDARY
        ) && self.hovered_item.as_deref() == Some(hover_key.as_str());
        let hover_this = cx.entity();
        let hover_key_hover = hover_key.clone();
        let row = row.on_hover(move |hovered, _, cx| {
            let key = hover_key_hover.clone();
            hover_this.update(cx, |root, cx| {
                root.hovered_item = hovered.then_some(key);
                cx.notify();
            });
        });
        let row = if shimmering {
            let base = rarity_color(&item.rarity, pal) & 0xff_ffff;
            row.with_animation(
                SharedString::from(format!("shimmer-{hover_key}")),
                Animation::new(std::time::Duration::from_secs(4)).repeat(),
                move |el, t| {
                    let hsla: gpui::Hsla = gpui::rgba((base << 8) | 0xff).into();
                    // Бриллиантовое переливание: играет ЯРКОСТЬ
                    // цвета редкости — оттенок не меняется, других
                    // цветов нет (заказчик).
                    let l =
                        (hsla.l + 0.18 * (2.0 * std::f32::consts::PI * t).sin()).clamp(0.0, 1.0);
                    el.border_color(gpui::hsla(hsla.h, hsla.s, l, 1.0))
                        .shadow(vec![gpui::BoxShadow {
                            color: gpui::hsla(hsla.h, hsla.s, l, 0.35),
                            offset: gpui::point(px(0.0), px(0.0)),
                            blur_radius: px(8.0),
                            spread_radius: px(0.0),
                            inset: true,
                        }])
                },
            )
            .into_any_element()
        } else {
            row.into_any_element()
        };
        let row = ContextMenu::new(format!("ld2-equip-ctx-{slot}"), menu)
            .manual(request)
            .child(row);
        // Тултип описания (заказчик 2026-11-20): через 1с наведения;
        // свечение медленно пульсирует наружу (~2.5 с) — пульс на
        // карточке внутри тултипа.
        let pulse_rarity = tip_rarity.clone();
        let row = TooltipTrigger::new(format!("ld2-equip-tip-{slot}"), row, move |_, _| {
            let pulse_rarity = pulse_rarity.clone();
            let hover_key = hover_key.clone();
            let pal = crate::palette::palette(&tip_theme);
            // Фон и вид — как у окна описания; отрицательные
            // поля гасят паддинг сурфейса.
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
                .border_color(rgb(rarity_color(&tip_rarity, &pal)))
                .text_color(rgb(pal.text))
                .text_size(px(crate::ui_tokens::FONT_MD))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(tip_name.clone()),
                )
                .child(item_description_body(
                    tip_lang,
                    &pal,
                    &tip_detail,
                    &tip_skills,
                ))
                .with_animation(
                    SharedString::from(format!("tip-pulse-{hover_key}")),
                    Animation::new(std::time::Duration::from_millis(2500)).repeat(),
                    move |card, t| {
                        let alpha: f32 = (0.30 + 0.20 * (2.0 * std::f32::consts::PI * t).sin())
                            .clamp(0.10, 0.50);
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
        .delay(std::time::Duration::from_secs(1));
        row.into_any_element()
    }

    /// Действия предмета — контекстное меню Ely (ФР-07/09/10, egui-паритет).
    pub(crate) fn item_actions(
        &mut self,
        cx: &mut Context<'_, Self>,
        _pal: &Palette,
        ch: &CharacterState,
        item: &ItemSnapshot,
    ) -> Menu {
        let lang = self.lang;
        let mut menu = Menu::new();

        // Использовать (свиток/зелье/…); M18/F54: камень усиления —
        // отдельный ack (Act::Sharpen) + открытие окна заточки.
        let uid = item.uid.clone();
        let is_stone = item.item_type == "upgrade_stone";
        menu = menu.item(menu_item(cx, t(lang, Key::MenuUse), move |this, _cx| {
            let cmd = ItemUse {
                item_uid: uid.clone(),
                target_id: None,
            };
            this.send_cmd(&cmd, if is_stone { Act::Sharpen } else { Act::Drop });
            if is_stone {
                // Геометрия карточки (гейт рендера — sharpen_stone из ack).
                this.open_popup(PopupKind::Upgrade);
            }
        }));

        // M17/F30a/F30c: инкрустация руны ИЛИ самоцвета — окно
        // выбора предмета + каст-бар.
        if matches!(
            item.item_type.as_str(),
            domain::ITEM_RUNE | domain::ITEM_GEM
        ) {
            let uid = item.uid.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::MenuInscribe),
                move |this, _cx| {
                    this.store.open_inscribe(uid.clone());
                },
            ));
        }

        // M17/F30b: экипировка — по слоту предмета; сервер — истина (§7).
        if let Some(slot) = slot_for(item, ch) {
            let equipped = ch.equip.values().flatten().any(|u| u == &item.uid);
            if !equipped {
                let uid = item.uid.clone();
                menu = menu.item(menu_item(cx, t(lang, Key::MenuEquip), move |this, _cx| {
                    let cmd = ItemEquip {
                        item_uid: uid.clone(),
                        slot: slot.clone(),
                    };
                    this.send_cmd(&cmd, Act::Equip);
                }));
            }
        }

        // M18 (указание заказчика): пункт «Инкрустация» — в разделе
        // экипировки (после «Надеть»), если есть слоты рун или
        // самоцветов — **свободные или занятые** (любой слот слотом;
        // пустой список — не показываем).
        let has_sockets = item
            .sockets
            .as_ref()
            .is_some_and(|sockets| !sockets.is_empty());
        if has_sockets {
            let uid = item.uid.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::BtnSocketing),
                move |this, _cx| {
                    // Окно сокетов — popup-диалог (аналогично
                    // описанию/заточке); соседние попапы закрываем.
                    this.store.item_detail = None;
                    this.store.close_upgrade();
                    this.upgrade_progress = None;
                    this.store.open_socketing(uid.clone());
                    this.open_popup(PopupKind::Socketing);
                },
            ));
        }

        // Улучшение (ФР-10): до максимума из конфига сервера.
        // M18/F54: сервер требует активный камень — пункт показываем
        // только когда камень уже выбран (иначе `E_INGREDIENTS`).
        let max_level = self
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let upgradable = matches!(
            item.item_type.as_str(),
            domain::ITEM_WEAPON | "armor" | domain::ITEM_STAFF
        ) && self.store.sharpen_stone.is_some();
        if upgradable && item.upgrade < max_level {
            let uid = item.uid.clone();
            let name = item.name.clone();
            menu = menu.item(menu_item(cx, t(lang, Key::UpgradeBtn), move |this, _cx| {
                // M18/F53: раньше модал-диспетчер показывал одну модалку
                // за раз — теперь попапы, закрываем соседнее вручную.
                this.store.item_detail = None;
                this.store.open_upgrade(uid.clone(), name.clone());
                // M18/F53: карточка заточки — как окно персонажа
                // (базовый размер/позиция, без затемнения).
                this.open_popup(PopupKind::Upgrade);
                let cmd = UpgradePreview {
                    item_uid: uid.clone(),
                };
                this.send_cmd(&cmd, Act::Preview);
            }));
        }

        // Описание / Выбросить.
        let detail = serde_json::to_value(item).unwrap_or_default();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDesc), move |this, _cx| {
            this.store.close_upgrade();
            this.upgrade_progress = None;
            this.store.item_detail = Some(detail.clone());
            this.open_popup(PopupKind::Item);
        }));
        let uid = item.uid.clone();
        let max_qty = item.qty.unwrap_or(1);
        let item_name = item.name.clone();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDrop), move |this, _cx| {
            // R-dnd (заказчик 2026-11-21): подтверждение выброса — окно
            // «Выбросить предмет?» (для стаков >1: инпут + ползунок +
            // «Все»; одна штука — сразу кнопка выброса).
            this.drop_confirm = Some(crate::root::DropPending {
                uid: uid.clone(),
                name: item_name.clone(),
                from: crate::root::DragSource::Bp,
                max: max_qty,
                qty: max_qty,
            });
        }));
        menu
    }

    /// Базовая карточка слота: рамка, подсветка редкости, подпись,
    /// drop-зона (RF3l: часть `equip_row`, лимит 200 строк).
    fn equip_card(
        &mut self,
        slot: &str,
        snap: &ld2_protocol::evt::StateSnapshot,
        lang: ld2_core::l10n::Lang,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> (
        gpui::Stateful<gpui::Div>,
        Option<ld2_protocol::types::ItemSnapshot>,
    ) {
        let uid = snap.char.equip.get(slot).cloned().flatten();
        let item = uid
            .as_ref()
            .and_then(|u| snap.char.inventory.iter().find(|i| i.uid == *u));
        // M18 (указание заказчика): карточка слота — вертикально:
        // **название слота в верхней строке**, предмет ниже (правым
        // краем); кнопки «Снять»/«Описание» — в контекстном меню.
        // M18: id обязателен для hover-состояния (gpui отслеживает
        // наведение у идентифицированных элементов — подсветка
        // карточки; без id работал только mouse_down).
        // min_h + stretch — одинаковые высота/ширина карточек;
        // выделение цветом предмета раскидывается на детей через
        // наследование text_color (label/название/столбцы —
        // контрастные при hover карточки).
        let mut row = div()
            .id(format!("ld2-equip-card-{slot}"))
            // Одинаковые высота и ширина карточек (указание
            // заказчика): min_h + stretch поперёк колонки.
            .min_h(px(76.0))
            // Длинное название не расширяет ячейку: карточка — не
            // больше своей половины колонки (заказчик 2026-11-27).
            .min_w_0()
            .self_stretch()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            // Каждый слот визуально заметен — рамка в цвете
            // границ панели.
            .border_1()
            .border_color(rgb(pal.border))
            // Эпик/легендарка: статичное внутреннее свечение в
            // покое (анимация — только по наведению, ниже).
            .when_some(item, |row, item| {
                row.shadow(rarity_glow_inset(&item.rarity, pal))
            })
            .cursor_pointer()
            // Подсветка: пустой слот — общий selection, занятый —
            // цветом предмета (ветки ниже); клик — контекстное меню.
            ;
        // Label слота: muted (контрастный текст при выделении
        // отменён заказчиком — выделяется только рамка).
        row = row.child(
            div()
                .text_size(px(crate::ui_tokens::FONT_SM))
                .text_color(rgb(pal.muted))
                .child(slot_label(lang, slot)),
        );
        // R-dnd: зона приёма (из рюкзака/из экипировки) — on_drop
        // по hover hit-test (работает для drag из любого места).
        let zone_slot = slot.to_string();
        let zone_this = cx.entity();
        let zone_slot_drop = zone_slot.clone();
        row = row
            .on_drop::<ItemDrag>(move |drag: &ItemDrag, _, cx| {
                zone_this.update(cx, |root, cx| {
                    let fits = drag.slot_req.as_deref() == Some(zone_slot_drop.as_str());
                    match &drag.from {
                        // Из рюкзака: подходящий слот → надеть;
                        // неподходящий — блок без предупреждения
                        // (предмет остаётся в рюкзаке, заказчик).
                        DragSource::Bp => {
                            if fits {
                                tracing::info!("dnd: drop → надеть {}", zone_slot_drop);
                                let cmd = ItemEquip {
                                    item_uid: drag.uid.clone(),
                                    slot: zone_slot_drop.clone(),
                                };
                                root.send_cmd(&cmd, Act::Equip);
                            } else {
                                tracing::info!("dnd: drop → блок, слот не подходит");
                            }
                        }
                        // Из экипировки: верный другой слот — перенести;
                        // неверный — вернуть в рюкзак (НЕ выбрасывать,
                        // заказчик).
                        DragSource::Equip(from_slot) => {
                            if *from_slot == zone_slot_drop {
                                tracing::info!("dnd: drop → тот же слот");
                            } else if fits {
                                tracing::info!("dnd: drop → надеть {}", zone_slot_drop);
                                let cmd = ItemEquip {
                                    item_uid: drag.uid.clone(),
                                    slot: zone_slot_drop.clone(),
                                };
                                root.send_cmd(&cmd, Act::Equip);
                            } else {
                                tracing::info!("dnd: drop → снять в рюкзак");
                                let cmd = ItemUnequip {
                                    slot: from_slot.clone(),
                                };
                                root.send_cmd(&cmd, Act::Unequip);
                            }
                        }
                    }
                    root.drag_item = None;
                    root.bp_hold = None;
                    cx.notify();
                });
            })
            // R-dnd: подсветка зоны при перетаскивании — drag_over
            // (hover hit-test; on_drag_move срабатывает только у
            // источника drag). Подходящий слот — зелёная, неверный —
            // красная 2px.
            .drag_over::<ItemDrag>({
                let slot_over = zone_slot.clone();
                let success = pal.success;
                let danger = pal.danger;
                move |st, drag: &ItemDrag, _, _| {
                    if drag.slot_req.as_deref() == Some(slot_over.as_str()) {
                        st.border_color(rgb(success))
                    } else {
                        st.border_color(rgb(danger)).border(px(2.0))
                    }
                }
            });
        (row, item.cloned())
    }

    /// Контекстное меню карточки слота (RF3l: часть `equip_row`).
    fn equip_card_menu(
        &mut self,
        cx: &mut Context<'_, Self>,
        lang: ld2_core::l10n::Lang,
        slot: &str,
        item: &ld2_protocol::types::ItemSnapshot,
    ) -> Menu {
        let mut menu = Menu::new();
        let detail = serde_json::to_value(item).unwrap_or_default();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDesc), move |this, _cx| {
            this.store.close_upgrade();
            this.upgrade_progress = None;
            this.store.item_detail = Some(detail.clone());
            this.open_popup(PopupKind::Item);
        }));
        let slot_for_cmd = slot.to_string();
        menu = menu.item(menu_item(
            cx,
            t(lang, Key::MenuUnequip),
            move |this, _cx| {
                let cmd = ItemUnequip {
                    slot: slot_for_cmd.clone(),
                };
                this.send_cmd(&cmd, Act::Unequip);
            },
        ));
        // M18 (указание заказчика): пункт «Инкрустация» — и в
        // разделе экипировки окна персонажа, если у вещи есть
        // слоты (пустые или занятые); тот же popup, что рюкзак.
        if item
            .sockets
            .as_ref()
            .is_some_and(|sockets| !sockets.is_empty())
        {
            let uid = item.uid.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::BtnSocketing),
                move |this, _cx| {
                    this.store.item_detail = None;
                    this.store.close_upgrade();
                    this.upgrade_progress = None;
                    this.store.open_socketing(uid.clone());
                    this.open_popup(PopupKind::Socketing);
                },
            ));
        }
        menu
    }

    /// Подпись и рамка карточки одетого предмета (RF3l: часть
    /// `equip_row`, лимит 200 строк).
    fn equip_card_title(
        &mut self,
        mut row: gpui::Stateful<gpui::Div>,
        item: &ld2_protocol::types::ItemSnapshot,
        _lang: ld2_core::l10n::Lang,
        pal: &Palette,
    ) -> gpui::Stateful<gpui::Div> {
        let color = item_color(item, pal);
        // Отступ-заполнитель прижимает предмет к правому краю.
        // M18/F58f: подпись как в рюкзаке (указание заказчика) —
        // слоты второй строкой под названием (`item_label_stack`).
        // M18 (указание заказчика): карточка одетого предмета —
        // ниже названия слота идёт **название предмета**, ниже —
        // **столбцы самоцветов/рун** (иконки слотов отдельной
        // строкой, левым краем).
        // Заказчик 2026-11-27: название предмета — в пределах
        // ячейки (не более половины колонки): не влезло — многоточие.
        // Бейджи +N/×N остаются видимыми, название ужимается первым.
        let mut name_line = div()
            .flex()
            .flex_row()
            .gap_1()
            .items_baseline()
            .min_w_0()
            .text_color(rgb(color))
            .child(
                div()
                    .min_w_0()
                    .flex_initial()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(item.name.clone()),
            );
        if item.upgrade > 0 {
            name_line = name_line.child(div().child(format!("+{}", item.upgrade)));
        }
        if let Some(qty) = item.qty.filter(|q| *q > 1) {
            name_line = name_line.child(div().child(format!("×{qty}")));
        }
        row = row.child(name_line);
        if let Some(sockets) = item.sockets.as_ref().filter(|s| !s.is_empty()) {
            row = row.child(
                div()
                    .text_color(rgb(color))
                    .child(socket_icons_row(item.item_type.as_str(), sockets)),
            );
        }
        // M18 (указание заказчика): выделение — **только рамка**
        // цветом предмета при наведении; фон и тексты не меняются
        // (контрастный текст отменён — баг при скролле).
        // Подсветка зоны при перетаскивании (заказчик): hover-стиль
        // должен уступать drop-состоянию, иначе рамка слота
        // перекрывается цветом предмета.
        row = row.hover(move |st| st.border_color(rgb(color)));
        row
    }
}

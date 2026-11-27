//! Окно персонажа: socket (RF3l).

use ld2_core::domain;

use super::*;

impl Root {
    /// Инкрустация посоха (ФР-09).
    /// M18: окно сокетов (инкрустация через меню оружия) — popup-диалог
    /// с шапкой и крестиком (раньше — модалка модал-диспетчера).
    pub(crate) fn socket_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let staff_uid = self.store.socketing.clone()?;
        // M18 (указание заказчика): окно «Инкрустация» — две страницы
        // (слоты → выбор вставки), на второй — кнопка «Назад» в шапке
        // (паттерн окна заточки).
        let title = t(self.lang, Key::SocketingTitle).to_string();
        let lang = self.lang;
        let leading = self.socket_pick.is_some().then(|| {
            div().child(
                IconButton::new("ld2-socket-back", IconName::ArrowLeft)
                    .tooltip(t(lang, Key::BtnBack))
                    .on_click(cx.listener(|root, _, _, cx| {
                        root.socket_pick = None;
                        cx.notify();
                    })),
            )
        });
        let inner = match self.socket_pick {
            Some(index) => self.pick_list_view(cx, pal, staff_uid, index),
            None => self.socket_view(cx, pal, staff_uid),
        };
        // Обёртка-флекс-колонка под ScrollArea — приём item_window (M18/F57).
        let body = div().flex().flex_col().flex_grow_1().min_h_0().child(
            ScrollArea::new("ld2-socketing-scroll")
                .vertical()
                .flex_grow_1()
                .min_h_0()
                .child(inner),
        );
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Socketing,
            title,
            leading,
            body,
            None,
        ))
    }

    pub(crate) fn socket_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        staff_uid: String,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div().size_full();
        };
        let Some(staff) = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == staff_uid)
            .cloned()
        else {
            tracing::info!("закрытие окна сокетов: staff не найден в инвентаре ({staff_uid})");
            self.store.close_socketing();
            return div().size_full();
        };
        // M18/F32: от оружия/посоха — только руны, от брони — только
        // самоцветы; у цели иного типа вставлять нечего.
        let insert_type = match staff.item_type.as_str() {
            domain::ITEM_WEAPON | domain::ITEM_STAFF => domain::ITEM_RUNE,
            "armor" => domain::ITEM_GEM,
            _ => "",
        };
        let runes: Vec<ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            // M18/F58f (баг): принимает слот — его `accepts`, а не тип
            // предмета (ADR-012): для цели (staff) с gem-слотами в списке
            // должны быть и самоцветы; заодно — только вставки, которым
            // есть свободный подходящий слот.
            .filter(|i| inscribe_target_possible(&i.item_type, &staff))
            // M18/F58f: уже вставленные руны не предлагаем (дубликаты
            // не стакаются — сервер и так откажет, §9).
            .filter(|i| {
                insert_type != domain::ITEM_RUNE
                    || !rune_already_inserted(&i.name, staff.sockets.iter().flatten())
            })
            .cloned()
            .collect();

        let mut col = div().flex().flex_col().gap_1();
        for socket in staff.sockets.iter().flatten() {
            let mut row = div().flex().flex_row().gap_2().items_center();
            // M18/F58f: маркер руна-слота — глиф Dwarven Runes
            // (пустой слот — свой глиф); слот самоцвета — ◈, а у
            // **инкрустированного** — знак его раритета (Symbola):
            // указание заказчика — глиф по рангу рядом со selfцветом.
            if socket.accepts.as_deref() == Some(domain::ITEM_GEM) {
                // M18 (указание заказчика): **пустой** слот после
                // извлечения камня — пустой кружочек (как у рунного),
                // а не ромбик ◈ (читался как занятый).
                let marker = match &socket.rune {
                    Some(rune) => match rune.rarity.as_deref() {
                        Some(rarity) => crate::gem_glyph::gem_symbol_div(rarity),
                        None => div().child("◈"),
                    },
                    None => div().child("\u{25CB}"),
                };
                row = row.child(marker);
                if socket.rune.is_none() {
                    row = row.child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(socket.index.to_string()),
                    );
                }
            } else {
                row = row.child(match &socket.rune {
                    Some(rune) => rune_glyph::glyph_div_for(rune.glyph.as_deref(), &rune.name),
                    // M18 (указание заказчика): пустой рунный слот —
                    // **пустой квадратик**; индекс слота рядом с рунами
                    // не показывается (как и возле занятых рун).
                    None => rune_glyph::empty_slot_glyph_div(),
                });
            }
            match &socket.rune {
                Some(rune) => {
                    let index = socket.index;
                    // M17/F30c: самоцвет — без зарядов (заряды у рун).
                    let text = if rune.spell.is_empty() {
                        rune.name.clone()
                    } else {
                        format!("{} ({}/{})", rune.name, rune.charges, rune.max)
                    };
                    row = row.child(div().child(text));
                    let suid = staff.uid.clone();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::ExtractRune),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            let cmd = SocketExtract {
                                staff_uid: suid.clone(),
                                socket_index: index,
                            };
                            this.send_cmd(&cmd, Act::Socket);
                            let _ = cx;
                        },
                    ));
                }
                None => {
                    // M18/F58f (баг «слот принимает только руны»): слот
                    // принимает по СВОЕМУ accepts — в рунный слот не
                    // предлагаем самоцветы и наоборот (ADR-012); у старого
                    // сервера поля нет — фолбэк по типу цели (§11).
                    let slot_inserts: Vec<&ItemSnapshot> = runes
                        .iter()
                        .filter(|i| match socket.accepts.as_deref() {
                            Some(accepts) => i.item_type == accepts,
                            None => inscribe_target_allowed(&i.item_type, staff.item_type.as_str()),
                        })
                        .collect();
                    let index = socket.index;
                    if slot_inserts.is_empty() {
                        row = row.child(
                            div()
                                .text_color(rgb(pal.muted))
                                .child(t(lang, Key::NoRunes)),
                        );
                    } else {
                        // M18 (указание заказчика): клик по пустому слоту
                        // открывает **новое окно** со списком самоцветов
                        // или рун для этого слота (по типу магазина в
                        // диалогах) — `pick_window`.
                        // M18 (указание заказчика): подсказка — по типу
                        // слота (самоцвет/руна), не многоточие.
                        let hint = match socket.accepts.as_deref() {
                            Some(domain::ITEM_GEM) => t(lang, Key::PickGem),
                            Some(_) => t(lang, Key::PickRune),
                            None => t(
                                lang,
                                if staff.item_type == "armor" {
                                    Key::PickGem
                                } else {
                                    Key::PickRune
                                },
                            ),
                        };
                        row = row
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(pal.selection)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    // Вторая страница окна — список вставок
                                    // этого слота (кнопка «Назад» в шапке).
                                    this.socket_pick = Some(index);
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::ui_tokens::FONT_SM))
                                    .text_color(rgb(pal.muted))
                                    .child(hint),
                            );
                    }
                }
            }
            col = col.child(row);
        }

        col = col.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                tracing::info!("закрытие окна сокетов: кнопка «Выход»");
                this.store.close_socketing();
                let _ = cx;
            },
        ));
        col
    }

    /// M18 (указание заказчика): окно выбора вставки — список рун или
    /// самоцветов для выбранного слота, **по типу магазина в диалогах**
    /// (строки: слева предмет — справа действие); клик по строке —
    /// процесс инкрустации в этот слот, окно закрывается.
    pub(crate) fn pick_list_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        staff_uid: String,
        index: u32,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let Some(staff) = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == staff_uid)
            .cloned()
        else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let Some(socket) = staff.sockets.iter().flatten().find(|s| s.index == index) else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::NoRunes));
        };
        // Список: принимает ли слот вставку (accepts; §11 — фолбэк по
        // типу цели), для рун — без уже вставленных (дубликаты).
        let inserts: Vec<ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|i| match socket.accepts.as_deref() {
                Some(accepts) => i.item_type == accepts,
                None => inscribe_target_allowed(&i.item_type, staff.item_type.as_str()),
            })
            .filter(|i| {
                i.item_type != domain::ITEM_RUNE
                    || !rune_already_inserted(&i.name, staff.sockets.iter().flatten())
            })
            .cloned()
            .collect();

        // Строки в стиле магазина (game::shop_view): панель, подсветка
        // наведения, клик по строке → действие.
        let mut col = div().flex().flex_col().gap_1().flex_grow_1().min_h_0();
        if inserts.is_empty() {
            col = col.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoRunes)),
            );
        }
        for item in &inserts {
            let pick = item.uid.clone();
            let pick_staff = staff_uid.clone();
            let row_id = format!("ld2-pick-{}", item.uid);
            col = col.child(
                div()
                    .id(row_id)
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .cursor_pointer()
                    .hover(move |st| st.bg(rgb(pal.selection)))
                    .child(item_label_row(item, rarity_color(&item.rarity, pal)))
                    .child(
                        div()
                            .text_size(px(crate::ui_tokens::FONT_SM))
                            .text_color(rgb(pal.muted))
                            .child(t(lang, Key::InsertRune)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Процесс инкрустации: вставка в выбранный слот.
                        let cmd = SocketInsert {
                            staff_uid: pick_staff.clone(),
                            socket_index: index,
                            rune_uid: pick.clone(),
                        };
                        this.send_cmd(&cmd, Act::Socket);
                        this.socket_pick = None;
                        cx.notify();
                    })),
            );
        }
        col
    }
}

//! Магазин и обучение (RF3m).

use super::*;

impl Root {
    /// M18/F39: панель магазина внутри окна диалога: вкладки валют
    /// торговца, баланс по активной валюте, товары с ценами (сервер
    /// считает наценки/скидки), покупка цветом показывает нехватку
    /// средств (сервер всё равно авторитет — `E_NOT_ENOUGH_MONEY`).
    pub(crate) fn shop_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        dialog: &NpcDialogView,
    ) -> Div {
        let lang = self.lang;
        let Some(shop) = dialog.shop.clone() else {
            return div().size_full();
        };
        let active = dialog
            .shop_currency
            .as_deref()
            .and_then(|currency| shop.tabs.iter().find(|tab| tab.currency == currency))
            .or_else(|| shop.tabs.first());
        let Some(tab) = active else {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::ShopEmpty));
        };

        // Вкладки валют торговца.
        let choices: Vec<Choice> = shop
            .tabs
            .iter()
            .map(|tab| Choice::new(tab.currency.as_str(), tab.currency_name.clone()))
            .collect();
        let this = cx.entity();
        let tabs = Tabs::new("ld2-shop-tabs", choices, tab.currency.as_str()).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    if let Some(view) = root.store.npc_dialog.as_mut() {
                        view.shop_currency = Some(value);
                    }
                    cx.notify();
                });
            },
        );

        // Баланс по активной валюте — из снапшота.
        let balance: i64 = self
            .store
            .snapshot
            .as_ref()
            .map(|snap| {
                snap.char
                    .inventory
                    .iter()
                    .filter(|item| item.def_id.as_deref() == Some(tab.currency.as_str()))
                    .map(|item| item.qty.unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);

        // M18/F41: колонка на весь бокс — гибкий отступ прижимает
        // кнопку возврата к нижнему краю панели.
        let mut col = div().flex().flex_col().gap_1().size_full();
        // M18/F41: сверху подпись, что вкладка — это валюта торговца.
        col = col.child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::ShopCurrency)),
        );
        col = col.child(tabs);
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(tf(lang, Key::ShopOwned, &[("n", &balance.to_string())])),
        );
        for item in &tab.items {
            let afford = balance >= item.price;
            let mut right = div().flex().flex_row().gap_2().items_center();
            right = right.child(
                div()
                    .text_size(px(13.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(if afford { pal.text } else { pal.danger }))
                    .child(format!("{}", item.price)),
            );
            let entity_id = dialog.id.clone();
            let currency = tab.currency.clone();
            let def_id = item.def_id.clone();
            right = right.child(act_btn_hover(
                cx,
                t(lang, Key::BtnBuy),
                pal.panel,
                pal.selection,
                if afford { pal.text } else { pal.danger },
                move |this, cx| {
                    let cmd = ld2_protocol::cmd::ShopBuy {
                        entity_id: entity_id.clone(),
                        currency: currency.clone(),
                        def_id: def_id.clone(),
                    };
                    this.send_cmd(&cmd, Act::ShopBuy);
                    let _ = cx;
                },
            ));
            let mut left = div().flex().flex_row().gap_2().items_center();
            left = left.child(div().text_size(px(13.0)).child(item.name.clone()));
            if item.qty > 1 {
                left = left.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("×{}", item.qty)),
                );
            }
            // M18/F40: строка товара — подсветка наведения, клик
            // открывает карточку информации (тот же item_detail, что
            // «Описание» в меню предмета).
            let row_id = format!("ld2-shop-item-{}", item.def_id);
            let detail = item.item.clone();
            col = col.child(
                div()
                    .id(row_id)
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .px_1()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(pal.selection)))
                    .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(snapshot) = &detail {
                            this.store.close_upgrade();
                            this.upgrade_progress = None;
                            this.store.item_detail = serde_json::to_value(snapshot).ok();
                            // M18/F53: описание предмета — карточка-попап.
                            this.open_popup(PopupKind::Item);
                        }
                        cx.notify();
                    }))
                    .child(left)
                    .child(right),
            );
        }
        // M18/F41: «К диалогу» — отступ растягивает колонку, кнопка
        // прижата к низу слева (дублирует иконку в шапке окна).
        col = col.child(div().flex_grow_1().min_h_0());
        col = col.child(
            div()
                .flex()
                .flex_row()
                .justify_start()
                .pt_1()
                .child(act_btn_hover(
                    cx,
                    t(lang, Key::ShopBack),
                    pal.panel,
                    pal.selection,
                    pal.text,
                    |this, cx| {
                        if let Some(view) = this.store.npc_dialog.as_mut() {
                            view.shop = None;
                            view.shop_currency = None;
                        }
                        let _ = cx;
                    },
                )),
        );
        col
    }
}

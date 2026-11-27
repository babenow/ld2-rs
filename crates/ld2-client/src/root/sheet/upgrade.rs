//! Окно персонажа: upgrade (RF3l).

use super::*;

impl Root {
    /// M18/F53: окно заточки — карточка-попап (как окно персонажа):
    /// шапка с названием предмета и крестиком, без затемняющего
    /// overlay; прогресс-бар и результат — внутри карточки.
    pub(crate) fn upgrade_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        // M18/F55: гейт окна — флаг «список показан», а не наличие
        // камня/предмета: последний камень потрачен → sharpen_stone
        // снят, но окно обязано пережить это, иначе «назад» закрывал
        // его вовсе (жалоба M18/F55).
        if !self.store.sharpen_list {
            return None;
        }
        let stone = self.store.sharpen_stone.clone();
        let view = self.store.upgrade.clone();
        let (title, leading, body) = match view {
            Some(view) => (
                view.item_name.clone(),
                // «Назад» к списку — как в окне диалога NPC (M18/F41).
                Some(self.sharpen_back_btn(cx)),
                self.upgrade_view(cx, pal, &view),
            ),
            None => (
                stone.map(|st| st.name).unwrap_or_default(),
                None,
                self.sharpen_list_view(cx, pal),
            ),
        };
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Upgrade,
            title,
            leading,
            body,
            None,
        ))
    }

    /// Улучшение (ФР-10): «сейчас +N → после +M», шанс, стоимость (§7).
    pub(crate) fn upgrade_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        view: &UpgradeView,
    ) -> Div {
        let lang = self.lang;
        let pending = self.store.pending == Some(Act::Upgrade);
        let progress = self
            .upgrade_progress
            .as_ref()
            .filter(|p| p.0 == view.item_uid)
            .cloned();
        let mut col = div().flex().flex_col().gap_1();
        let Some(preview) = view.preview.clone() else {
            if let Some(res) = view.result.as_ref() {
                // M18/F55: итог броска — прямо на экране заточки,
                // зелёный успех / красный провал (а не уведомление ОС).
                col = col.child(self.sharpen_result_line(lang, pal, res));
            } else {
                col = col.child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::Loading)),
                );
            }
            // M18/F53: кнопки внизу нет — закрытие крестиком в шапке.
            return col;
        };

        // M18/F55: итог прошлого броска остаётся виден на том же
        // экране (автопревью его не затирает) — гасится только при
        // старте следующего улучшения.
        if let Some(res) = view.result.as_ref() {
            col = col.child(self.sharpen_result_line(lang, pal, res));
        }

        let current = self
            .store
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.char
                    .inventory
                    .iter()
                    .find(|i| i.uid == view.item_uid)
                    .map(|i| i.upgrade)
            })
            .unwrap_or(0);
        let delta = preview["delta"]["upgrade"].as_str().unwrap_or("+?");
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(format!("+{current} → {delta}")),
        );
        if let Some(chance) = preview["chance"].as_f64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {:.0}%",
                t(lang, Key::ChanceLabel),
                chance * 100.0
            )));
        }
        if let Some(gold) = preview["cost"]["gold"].as_i64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {} зол.",
                t(lang, Key::CostLabel),
                gold
            )));
        }
        if let Some(materials) = preview["cost"]["materials"].as_object() {
            for (name, qty) in materials {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("  • {name}: {qty}")),
                );
            }
        }

        // M18/F53: что будет при неудаче — из capability `upgrade.rules`.
        let fail_rule = self
            .store
            .session
            .as_ref()
            .and_then(|s| s.features.get("upgrade.rules"))
            .and_then(|v| v.as_str())
            .unwrap_or("keep");
        let fail_text = match fail_rule {
            "level_down" => "При неудаче: уровень снижается на 1 (не ниже 0).",
            "break_on_fail" => "При неудаче: предмет разрушается.",
            _ => "При неудаче: уровень не меняется, предмет сохраняется.",
        };
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(fail_text),
        );

        let preview_id = preview["preview_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let can_start = !preview_id.is_empty() && !pending && progress.is_none();
        if let Some((_uid, _pid, started, duration)) = progress.clone() {
            let now = self.now_ms();
            let left = (started + duration - now).max(0);
            let frac = ((now - started) as f32 / duration.max(1) as f32).clamp(0.0, 1.0);
            let secs = (left + 999) / 1000;
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt(px(8.0))
                    .child(
                        div().h(px(6.0)).rounded_md().bg(rgb(pal.border)).child(
                            div()
                                .h_full()
                                .w(gpui::relative(frac))
                                .rounded_md()
                                .bg(rgb(pal.warn)),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(format!("{}… {} с", t(lang, Key::Loading), secs)),
                    ),
            );
        }
        // M18/F53: внизу только «Улучшить» — закрытие крестиком в шапке.
        let mut row = div().flex().flex_row().gap_2().mt(px(8.0));
        let uid = view.item_uid.clone();
        if can_start {
            row = row.child(act_btn(
                cx,
                t(lang, Key::UpgradeBtn),
                pal.warn,
                0xffffff,
                move |this, cx| {
                    // M18/F53: прогресс 3 с (из настроек сервера), затем
                    // отправка cmd/upgrade.start (root::tick).
                    let duration_ms = this
                        .store
                        .session
                        .as_ref()
                        .map(|s| (s.upgrade.duration_s * 1000.0).round() as i64)
                        .unwrap_or(3_000);
                    // M18/F55: прошлый итог не должен висеть во время
                    // прогресса нового броска.
                    if let Some(v) = this.store.upgrade.as_mut() {
                        v.result = None;
                    }
                    this.upgrade_progress =
                        Some((uid.clone(), preview_id.clone(), this.now_ms(), duration_ms));
                    // M24: единый прогресс-бар левой колонки —
                    // заточка, подпись «Действие · имя».
                    let item_name = this
                        .store
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.char.inventory.iter().find(|i| i.uid == uid))
                        .map(|i| i.name.clone())
                        .unwrap_or_default();
                    this.start_progress(
                        crate::root::ProgressSource::Sharpen,
                        format!("{} · {}", t(this.lang, Key::UpgradeBtn), item_name),
                        duration_ms,
                    );
                    let _ = cx;
                },
            ));
        } else {
            row = row.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(if pending {
                        t(lang, Key::Loading)
                    } else {
                        t(lang, Key::UpgradeBtn)
                    }),
            );
        }
        col = col.child(row);
        col
    }

    /// M18/F54: кнопка «назад» в шапке окна заточки — возврат от
    /// экрана заточки к списку предметов (само окно не закрывается).
    pub(crate) fn sharpen_back_btn(&mut self, cx: &mut Context<'_, Self>) -> Div {
        div().child(
            IconButton::new("ld2-sharpen-back", IconName::ArrowLeft)
                .tooltip(t(self.lang, Key::BtnBack))
                .on_click(cx.listener(|root, _, _, cx| {
                    root.store.close_upgrade();
                    root.upgrade_progress = None;
                    cx.notify();
                })),
        )
    }

    /// M18/F54: экран 1 — предметы, которые можно заточить (строки как
    /// в магазине торговца): название/уровень слева, «+N» справа;
    /// клик → экран заточки этого предмета.
    pub(crate) fn sharpen_list_view(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let max_level = self
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        // M18/F55: камень потрачен (последний) — список предметов не
        // показываем: без него сервер не даст превью. Вместо пустоты —
        // заглушка, чтобы было понятно, почему дальше идти некуда.
        if self.store.sharpen_stone.is_none() {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.danger))
                .child(t(lang, Key::SharpenNoStone));
        }
        // M18/F54: качество камня — фиксированная шапка **над**
        // списком (не уезжает при прокрутке), строки — в ScrollArea:
        // предметов больше, чем помещается в карточку.
        let header = self.store.sharpen_stone.clone().map(|stone| {
            let sign = if stone.quality >= 0 { "+" } else { "" };
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!(
                    "{}: {}{}%",
                    t(lang, Key::StatQuality),
                    sign,
                    stone.quality
                ))
        });
        let mut list = div().flex().flex_col().gap_1();
        let mut found = false;
        for item in &snap.char.inventory {
            // Все улучшаемые типы (M18/F54: камни — для всех), до потолка.
            if !matches!(item.item_type.as_str(), "weapon" | "armor" | "staff")
                || item.upgrade >= max_level
            {
                continue;
            }
            found = true;
            let uid = item.uid.clone();
            let name = item.name.clone();
            let upgrade = item.upgrade;
            list = list.child(
                div()
                    .id(format!("ld2-sharpen-{uid}"))
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
                    .hover(|s| s.bg(rgb(pal.selection)))
                    .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Экран 2: превью этого предмета (камень уже активен).
                        this.store.open_upgrade(uid.clone(), name.clone());
                        let cmd = UpgradePreview {
                            item_uid: uid.clone(),
                        };
                        this.send_cmd(&cmd, Act::Preview);
                        cx.notify();
                    }))
                    .child(item_label_row(item, item_color(item, pal)))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(format!("+{upgrade}")),
                    ),
            );
        }
        if !found {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::SharpenNoItems)),
            );
        }
        // Шапка (качество камня) закреплена, список прокручивается —
        // ScrollArea занимает остаток карточки (как окно NPC/персонажа).
        div()
            .flex()
            .flex_col()
            .gap_1()
            .size_full()
            .min_h_0()
            .children(header)
            .child(
                ScrollArea::new("ld2-sharpen-list")
                    .vertical()
                    .flex_grow_1()
                    .min_h_0()
                    .child(list),
            )
    }

    /// M18/F55: итог заточки для показа прямо на экране заточки —
    /// карточка зелёным при успехе и красным при провале (разрушение /
    /// без изменений). Вызывается и в ветке «превью ещё нет», и в
    /// основной — итог переживает автопревью.
    pub(crate) fn sharpen_result_line(
        &self,
        lang: Lang,
        pal: &Palette,
        res: &serde_json::Value,
    ) -> Div {
        let ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let (text, color) = if ok {
            let n = res.get("new_upgrade").and_then(|v| v.as_u64()).unwrap_or(0);
            (
                tf(lang, Key::SharpenResultOk, &[("s", &n.to_string())]),
                pal.success,
            )
        } else if res
            .get("item_destroyed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            (t(lang, Key::SharpenResultBroken).to_string(), pal.danger)
        } else {
            (t(lang, Key::SharpenResultFail).to_string(), pal.danger)
        };
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(color))
            .child(text)
    }

    /// M17/F30a: список оружия, куда можно инкрустировать руну —
    /// предметы с сокетами и хотя бы одним свободным; клик → каст-бар
    /// 3 с (root::start_inscribe), без отмены.
    pub(crate) fn inscribe_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        rune_uid: &str,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.as_ref() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_1();
        let mut found = false;
        // M18/F32: тип вставки — сам выбранный предмет (rune|gem).
        let insert_type = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == rune_uid)
            .map(|i| i.item_type.clone())
            .unwrap_or_default();
        for item in &snap.char.inventory {
            // M18/F58: принимает ли слот предмета (accepts) — заменяет
            // прежнее правило по типу предмета + проверку свободного
            // слота; старый сервер — фолбэк (§11).
            if !inscribe_target_possible(&insert_type, item) {
                continue;
            }
            found = true;
            let rune = rune_uid.to_string();
            let uid = item.uid.clone();
            col = col.child(
                div()
                    .id(format!("ld2-inscribe-{uid}"))
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_inscribe(rune.clone(), uid.clone());
                        cx.notify();
                    }))
                    .child(item_label_row(item, item_color(item, pal))),
            );
        }
        if !found {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::InscribeNoWeapons)),
            );
        }
        col
    }
}

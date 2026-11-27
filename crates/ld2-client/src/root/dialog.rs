//! M20: окно игрового диалога — **заготовка** (план M20): как
//! обычные диалоги, но с кнопками действий, многостраничностью и
//! кнопкой «Назад»; execute-диалог — поля ввода, отправляющие набор
//! вводов персонажа (`cmd/dialog.submit`).
//!
//! Навигация заготовки: действие `close` — закрыть окно, `go:<page>`
//! — перейти на страницу (серверные переходы подключатся вместе с
//! контентом игровых диалогов); всё остальное — только
//! `cmd/dialog.choice` серверу.

use ely::buttons::IconButton;
use ely::forms::{Input, NumberInput, TextInput};
use ely::primitives::IconName;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Window};
use ld2_core::l10n::{t, Key};
use ld2_protocol::cmd::{DialogChoice, DialogSubmit};
use ld2_protocol::evt::{DialogAction, DialogOpen, DialogPage};

use crate::root::{act_btn, Act, Palette, PopupKind, Root};

impl Root {
    /// M20: закрыть игровой диалог (крестик/Esc/скрим/действие
    /// `close`/после отправки ввода) — гейт рендера `store.dialog`.
    pub(super) fn close_game_dialog(&mut self) {
        self.store.dialog = None;
        self.dialog_seen = None;
        self.dialog_inputs = None;
        self.dialog_page = 0;
        self.dialog_values.clear();
        self.popup_mut(PopupKind::Dialog).open = false;
    }

    /// M20: окно диалога — popup-карточка (`evt/dialog/open` — гейт).
    pub(super) fn dialog_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let dialog = self.store.dialog.clone()?;
        // Новый диалог → сброс страницы и полей.
        if self.dialog_seen.as_deref() != Some(dialog.id.as_str()) {
            self.dialog_seen = Some(dialog.id.clone());
            self.dialog_page = 0;
            self.dialog_values.clear();
            self.dialog_inputs = None;
            for inp in &dialog.inputs {
                self.dialog_values
                    .insert(inp.id.clone(), inp.default.clone());
            }
            self.popup_mut(PopupKind::Dialog).open = true;
        }
        // Text-поля execute — Entity<TextInput> (создаются под диалог;
        // NumberInput состояния не хранит — значения в dialog_values).
        if dialog.kind == "execute"
            && self
                .dialog_inputs
                .as_ref()
                .is_none_or(|(id, _)| id != &dialog.id)
        {
            let mut fields = Vec::new();
            for inp in dialog.inputs.iter().filter(|i| i.kind != "number") {
                let label = inp.label.clone();
                let default = inp.default.clone();
                let entity = cx.new(|cx| {
                    let mut ti = TextInput::new(window, cx).placeholder(label);
                    if !default.is_empty() {
                        ti.set_text(default, cx);
                    }
                    ti
                });
                fields.push((inp.id.clone(), entity));
            }
            self.dialog_inputs = Some((dialog.id.clone(), fields));
        }
        // «Назад» — в шапке (паттерн окна инкрустации), только если
        // есть куда возвращаться.
        let lang = self.lang;
        let leading = (dialog.kind == "page" && self.dialog_page > 0).then(|| {
            div().child(
                IconButton::new("ld2-dialog-back", IconName::ArrowLeft)
                    .tooltip(t(lang, Key::BtnBack))
                    .on_click(cx.listener(|root, _, _, cx| {
                        root.dialog_page = root.dialog_page.saturating_sub(1);
                        cx.notify();
                    })),
            )
        });
        let title = dialog.title.clone();
        let body = self.dialog_body(cx, pal, &dialog);
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Dialog,
            title,
            leading,
            body,
            None,
        ))
    }

    /// Тело окна: страница с действиями либо поля execute + отправка.
    fn dialog_body(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        dialog: &DialogOpen,
    ) -> Div {
        let lang = self.lang;
        let mut col = div().flex().flex_col().gap_3().px_3().py_3();
        if dialog.kind == "execute" {
            for inp in &dialog.inputs {
                col = col.child({
                    let mut row = div().flex().flex_col().gap_1();
                    row = row.child(div().text_color(rgb(pal.muted)).child(inp.label.clone()));
                    row = row.child(if inp.kind == "number" {
                        let this = cx.entity();
                        let key = inp.id.clone();
                        let value: f64 = self
                            .dialog_values
                            .get(&inp.id)
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0.0);
                        div().w(px(200.0)).child({
                            NumberInput::new(format!("ld2-dialog-num-{}", inp.id), value)
                                .precision(0)
                                .step(1.0)
                                .on_change(move |v, _window, acx| {
                                    this.update(acx, |root, cx| {
                                        root.dialog_values
                                            .insert(key.clone(), format!("{}", v.round() as i64));
                                        cx.notify();
                                    });
                                })
                        })
                    } else {
                        let entity = self.dialog_inputs.as_ref().and_then(|(_, fields)| {
                            fields
                                .iter()
                                .find(|(id, _)| id == &inp.id)
                                .map(|(_, entity)| entity.clone())
                        });
                        match entity {
                            Some(entity) => div().w(px(280.0)).child(Input::new(&entity)),
                            None => div(),
                        }
                    });
                    row
                });
            }
            col = col.child(act_btn(
                cx,
                t(lang, Key::DialogSend).to_string(),
                pal.accent,
                pal.accent_text,
                |root, cx| {
                    root.submit_dialog(cx);
                },
            ));
        } else {
            let idx = self.dialog_page.min(dialog.pages.len().saturating_sub(1));
            self.dialog_page = idx;
            if let Some(page) = dialog.pages.get(idx) {
                col = col.child(
                    div()
                        .text_size(px(crate::ui_tokens::FONT_LG))
                        .child(page.text.clone()),
                );
                let mut actions = div().flex().flex_row().gap_2().items_center().flex_wrap();
                for action in &page.actions {
                    actions = actions.child(Self::dialog_action_btn(cx, pal, dialog, page, action));
                }
                col = col.child(actions);
            }
        }
        col
    }

    /// Кнопка действия страницы: `cmd/dialog.choice` + навигация
    /// заготовки (`close` / `go:<page_id>`).
    fn dialog_action_btn(
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        dialog: &DialogOpen,
        page: &DialogPage,
        action: &DialogAction,
    ) -> Div {
        let dialog_id = dialog.id.clone();
        let page_id = page.id.clone();
        let action_id = action.id.clone();
        let label = action.label.clone();
        act_btn(cx, label, pal.panel, pal.text, move |root, cx| {
            let cmd = DialogChoice {
                dialog_id: dialog_id.clone(),
                page_id: page_id.clone(),
                action_id: action_id.clone(),
            };
            root.send_cmd(&cmd, Act::DialogChoice);
            if action_id == "close" {
                root.close_game_dialog();
            } else if let Some(target) = action_id.strip_prefix("go:") {
                if let Some(i) = root
                    .store
                    .dialog
                    .as_ref()
                    .and_then(|d| d.pages.iter().position(|p| p.id == target))
                {
                    root.dialog_page = i;
                }
            }
            cx.notify();
        })
    }

    /// Отправка ввода execute-диалога: number — из `dialog_values`,
    /// text — из `Entity<TextInput>`; после ack окно закрывается.
    fn submit_dialog(&mut self, cx: &mut Context<'_, Self>) {
        let Some(dialog) = self.store.dialog.clone() else {
            return;
        };
        let mut values = serde_json::Map::new();
        for inp in &dialog.inputs {
            if inp.kind == "number" {
                let n: i64 = self
                    .dialog_values
                    .get(&inp.id)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                values.insert(inp.id.clone(), serde_json::json!(n));
            } else {
                let text = self
                    .dialog_inputs
                    .as_ref()
                    .and_then(|(_, fields)| {
                        fields
                            .iter()
                            .find(|(id, _)| id == &inp.id)
                            .map(|(_, entity)| entity.read(cx).text().to_string())
                    })
                    .unwrap_or_default();
                values.insert(inp.id.clone(), serde_json::json!(text));
            }
        }
        let cmd = DialogSubmit {
            dialog_id: dialog.id.clone(),
            values,
        };
        // M24: submit админ-диалога количества — содержимое цели
        // правой колонки устаревает (свой рюкзак обновляют события
        // инвентаря, потому для self перезапрос не нужен).
        if cmd.dialog_id.starts_with("admin.")
            && self.admin_popup.open
            && self.admin_target != "self"
        {
            self.admin_target_request = true;
        }
        self.send_cmd(&cmd, Act::DialogSubmit);
        self.close_game_dialog();
        cx.notify();
    }
}

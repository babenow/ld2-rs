//! M20: окно выбора количества — **переиспользуемый** компонент
//! (выброс предмета; в будущем — покупка и т.п., по указанию
//! заказчика): поле ввода цифр (`NumberInput`) + ползунок (`Slider`)
//! для донастройки, кнопки действия и отмены.

use ely::forms::{NumberInput, Slider};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Window};
use ld2_core::l10n::{t, Key};

use crate::root::{act_btn, Palette, PopupKind, Root};

/// Что делать после подтверждения (заготовка: расширять вариантами —
/// Buy и т.п.).
#[derive(Debug, Clone)]
#[allow(dead_code)] // окно количества больше не открывается (dnd-подтверждение)
pub enum QuantityAction {
    Drop { uid: String },
}

/// Состояние открытого окна.
#[derive(Debug, Clone)]
pub struct QuantityPick {
    pub action: QuantityAction,
    pub item_name: String,
    /// Максимум (сколько есть в рюкзаке).
    pub max: i64,
    /// Выбранное количество (1..=max).
    pub qty: i64,
}

impl Root {
    /// M20: тело окна — цифры + ползунок + «Выбросить»/«Отмена».
    pub(super) fn quantity_body(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let pick = match self.quantity_pick.clone() {
            Some(pick) => pick,
            None => return div(),
        };
        let max = pick.max.max(1) as f64;
        let qty = pick.qty.clamp(1, pick.max) as f64;

        let mut col = div().flex().flex_col().gap_3().px_3().py_3();
        col = col.child(
            div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::QtyLabel)),
        );
        // Поле ввода цифр.
        col = col.child(div().w(px(180.0)).child({
            let this = cx.entity();
            NumberInput::new("ld2-quantity", qty)
                .precision(0)
                .step(1.0)
                .range(1.0, max)
                .on_change(move |v, _window, acx| {
                    this.update(acx, |root, cx| {
                        if let Some(pick) = root.quantity_pick.as_mut() {
                            let v = v.round() as i64;
                            pick.qty = v.clamp(1, pick.max);
                        }
                        cx.notify();
                    });
                })
        }));
        // Ползунок — та же величина (синхрон через состояние окна).
        col = col.child(
            Slider::new("ld2-quantity-slider", qty)
                .range(1.0, max)
                .step(1.0)
                .on_change({
                    let this = cx.entity();
                    move |v, _window, acx| {
                        this.update(acx, |root, cx| {
                            if let Some(pick) = root.quantity_pick.as_mut() {
                                let v = v.round() as i64;
                                pick.qty = v.clamp(1, pick.max);
                            }
                            cx.notify();
                        });
                    }
                }),
        );
        // Кнопки: действие + отмена.
        let mut buttons = div().flex().flex_row().gap_2().items_center();
        buttons = buttons.child(act_btn(
            cx,
            t(lang, Key::MenuDrop).to_string(),
            pal.accent,
            pal.accent_text,
            |root, cx| {
                let pick = root.quantity_pick.take();
                let Some(pick) = pick else { return };
                match pick.action {
                    QuantityAction::Drop { uid } => {
                        let cmd = ld2_protocol::cmd::ItemDrop {
                            item_uid: uid,
                            qty: Some(pick.qty),
                        };
                        root.send_cmd(&cmd, crate::root::Act::Drop);
                    }
                }
                root.popup_mut(PopupKind::Quantity).open = false;
                let _ = cx;
            },
        ));
        buttons = buttons.child(act_btn(
            cx,
            t(lang, Key::CancelBtn).to_string(),
            pal.panel,
            pal.text,
            |root, cx| {
                root.quantity_pick = None;
                root.popup_mut(PopupKind::Quantity).open = false;
                let _ = cx;
            },
        ));
        col = col.child(buttons);
        col
    }

    /// M20: окно выбора количества — popup-карточка (заголовок — имя
    /// предмета; тело — [`Self::quantity_body`]).
    pub(super) fn quantity_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let pick = self.quantity_pick.clone()?;
        let title = pick.item_name.clone();
        let body = self.quantity_body(cx, pal);
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Quantity,
            title,
            None,
            body,
            None,
        ))
    }
}

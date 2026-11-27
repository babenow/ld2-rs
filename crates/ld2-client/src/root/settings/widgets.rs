//! Строки настроек: подпись — ползунок — значение (M14/F8, M39).

use super::*;

/// M14/F8 (фикс смоука): слайдер Ely сам ширины не имеет — `Track`
/// рендерится `flex_none` с одним padding, а все его дети абсолютно
/// позиционированы (внешняя ширина ≈ thumb, ≈0 полезной площади: не
/// виден и не кликается). Обёртка — flex-**колонка**: поперечная ось
/// растягивает слайдер на всю ширину ячейки, а `size_full()` у Ely
/// получает реально определённую область.
pub(super) fn slider_cell(slider: Slider) -> Div {
    div().flex().flex_col().flex_grow_1().child(slider)
}

/// Показ текущего значения настройки справа от ползунка.
pub(super) fn value_readout(pal: &Palette, text: String) -> Div {
    div()
        .w(px(56.0))
        .text_size(px(crate::ui_tokens::FONT_MD))
        .text_color(rgb(pal.text))
        .child(text)
}

/// Параметры строки «подпись — ползунок — значение, px» (пакет —
/// иначе clippy `too_many_arguments`).
pub(super) struct PxRow {
    pub label: Key,
    pub id: &'static str,
    pub value: f32,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub set: fn(&mut Root, f32),
}

/// Строка «подпись — ползунок — значение, px» с диапазоном и шагом.
pub(super) fn px_row(cx: &mut Context<'_, Root>, pal: &Palette, lang: Lang, spec: PxRow) -> Div {
    let PxRow {
        label,
        id,
        value,
        min,
        max,
        step,
        set,
    } = spec;
    let this = cx.entity();
    div()
        .flex()
        .flex_row()
        .gap_2()
        .items_center()
        .child(
            div()
                .w(px(130.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, label)),
        )
        .child(slider_cell(
            Slider::new(id, f64::from(value))
                .range(f64::from(min), f64::from(max))
                .step(f64::from(step))
                .on_change(move |v, _window, acx| {
                    this.update(acx, |root, cx| {
                        set(root, v as f32);
                        cx.refresh_windows();
                        cx.notify();
                        root.save_config();
                    });
                }),
        ))
        .child(value_readout(pal, format!("{} px", value.round() as i32)))
}

//! gpui-клиент ld2 — спайк M10.1 (PLAN-M10): окно с экраном входа.
//!
//! Цель фазы: проверить git-pin gpui v1.26.2, рендер кириллицы на
//! embedded-шрифте (§13 ТЗ: фиксированный набор, не системные) и сборку
//! на 3 ОС (gate Windows). Функциональность (вход, store) — фазы M10.2+.

use std::borrow::Cow;

use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};
use gpui_platform::application;

/// Экран входа (статический макет — функциональность появится в M10.3).
struct LoginSpike {
    version: SharedString,
}

impl Render for LoginSpike {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .size_full()
            .justify_center()
            .items_center()
            .bg(rgb(0x1e1e24))
            .text_color(rgb(0xeceff4))
            .child(
                div()
                    .text_2xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("ld2 — вход в мир"),
            )
            // Статичные поля формы (паритет макета egui-экрана входа).
            .child(field("Логин"))
            .child(field("Пароль"))
            .child(
                div()
                    .px_6()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x4a90d9))
                    .text_color(rgb(0xffffff))
                    .child("Войти"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x8a8f98))
                    .child(format!("v{} · demo/demo123", self.version)),
            )
    }
}

fn field(label: &str) -> impl IntoElement {
    div()
        .w(px(280.0))
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(0x5a5f68))
        .bg(rgb(0x2a2a30))
        .text_color(rgb(0x9a9fa8))
        .child(label.to_string())
}

fn main() {
    application().run(|cx: &mut App| {
        // §13: embedded-шрифт с кириллицей (PT Sans, OFL) — без системных.
        let fonts = [
            Cow::Borrowed(include_bytes!("../assets/fonts/PT_Sans-Web-Regular.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../assets/fonts/PT_Sans-Web-Bold.ttf").as_slice()),
        ];
        if let Err(e) = cx.text_system().add_fonts(fonts.into()) {
            eprintln!("не удалось загрузить шрифты: {e:#}");
        }

        let bounds = Bounds::centered(None, size(px(640.0), px(480.0)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("ld2 (gpui)".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| LoginSpike {
                    version: env!("CARGO_PKG_VERSION").into(),
                })
            },
        )
        .expect("окно");
        cx.activate(true);
    });
}

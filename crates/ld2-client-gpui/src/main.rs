//! gpui-клиент ld2 — точка входа (M10.3: шелл, экраны, настройки).
//!
//! Логика игры — в `ld2-core` (D12): здесь только gpui-рендер, опрос
//! ingress-канала транспорта и исполнение эффектов (см. [`crate::root`]).

mod palette;
mod root;
mod text_input;

use std::borrow::Cow;
use std::time::Duration;

use gpui::{px, size, App, AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions};
use gpui_platform::application;
use ld2_core::config::ConfigStore;
use ld2_core::l10n::Lang;
use ld2_core::store::AppStore;
use ld2_core::transport::{self, TransportConfig};
use tokio::sync::mpsc;

use crate::root::{Root, Shell};
use crate::text_input::{
    Backspace, Copy, Cut, Delete, End, Home, Left, Paste, Right, SelectAll, SelectLeft,
    SelectRight, ShowCharacterPalette, Submit,
};

fn main() -> anyhow::Result<()> {
    let _log_guard = ld2_core::logging::init()?;
    tracing::info!("ld2-gpui v{} — старт", env!("CARGO_PKG_VERSION"));

    let store = ConfigStore::open()?;
    let config = store.load()?;
    tracing::info!("конфиг: host={}, lang={}", config.host, config.lang);
    let lang = Lang::from_code(&config.lang);

    // Keyring-resume (ФР-01): входили раньше — экран входа пропускаем.
    let mut tcfg = TransportConfig::new(config.host.clone());
    if let Some(login) = &config.last_login {
        if let Some(token) =
            ld2_core::secrets::get_token_with_timeout(&config.host, login, Duration::from_secs(1))
        {
            tcfg.refresh_token = Some(token);
            tracing::info!("keyring: resume-токен найден ({login})");
        }
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let transport = transport::spawn(tcfg, tx);

    // НФР «Обновления»: проверка GitHub Releases в фоне (не блокирует).
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    ld2_core::update_check::spawn_check(update_tx);

    let mut app_store = AppStore::new();
    // НФР §8/ФР-19: лимит истории чата — из конфига.
    app_store.chat_limit = config.chat_limit;

    let shell = Shell {
        store: app_store,
        config,
        config_store: store,
        lang,
        transport,
        ingress: rx,
        update_rx,
    };

    application().run(move |cx: &mut App| {
        // §13: embedded-шрифт с кириллицей (PT Sans, OFL) — без системных.
        let fonts = [
            Cow::Borrowed(include_bytes!("../assets/fonts/PT_Sans-Web-Regular.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../assets/fonts/PT_Sans-Web-Bold.ttf").as_slice()),
        ];
        if let Err(e) = cx.text_system().add_fonts(fonts.into()) {
            tracing::warn!("не удалось загрузить шрифты: {e:#}");
        }

        // Клавиши текстового ввода — только в контексте поля (ФР-19:
        // ремаппинг самих действий мира — M10.4).
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, Some("TextInput")),
            KeyBinding::new("delete", Delete, Some("TextInput")),
            KeyBinding::new("left", Left, Some("TextInput")),
            KeyBinding::new("right", Right, Some("TextInput")),
            KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
            KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
            KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
            KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
            KeyBinding::new("cmd-v", Paste, Some("TextInput")),
            KeyBinding::new("ctrl-v", Paste, Some("TextInput")),
            KeyBinding::new("cmd-c", Copy, Some("TextInput")),
            KeyBinding::new("ctrl-c", Copy, Some("TextInput")),
            KeyBinding::new("cmd-x", Cut, Some("TextInput")),
            KeyBinding::new("ctrl-x", Cut, Some("TextInput")),
            KeyBinding::new("home", Home, Some("TextInput")),
            KeyBinding::new("end", End, Some("TextInput")),
            KeyBinding::new("enter", Submit, Some("TextInput")),
            KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("TextInput")),
        ]);

        let root = cx.new(|cx| Root::new(cx, shell));

        // Навигация мира (ФР-03) по хоткеям из config.toml (ФР-19) —
        // логика в Root::handle_keystroke (модулю main поля Root не видны).
        let nav_root = root.clone();
        cx.observe_keystrokes(move |ev, window, cx| {
            nav_root.update(cx, |root, cx| {
                root.handle_keystroke(&ev.keystroke, window, cx);
            });
        })
        .detach();

        // Фоновый цикл: опрос транспорта с idle-backoff (НФР §8).
        let poller = root.clone();
        cx.spawn(async move |cx: &mut gpui::AsyncApp| {
            let mut delay = Duration::from_millis(50);
            loop {
                cx.background_executor().timer(delay).await;
                delay = poller.update(cx, |root, cx| root.tick(cx));
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("ld2 (gpui)".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, _| root.clone(),
        )
        .expect("окно");
        cx.activate(true);
    });
    Ok(())
}

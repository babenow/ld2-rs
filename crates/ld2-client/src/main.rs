//! gpui-клиент ld2 — точка входа (M10.3: шелл, экраны, настройки;
//! M11.1: gpui-pre + каркас Ely — ADR-004).
//!
//! Логика игры — в `ld2-core` (D12): здесь только gpui-рендер, опрос
//! ingress-канала транспорта и исполнение эффектов (см. [`crate::root`]).

mod palette;
mod root;

use std::borrow::Cow;
use std::time::Duration;

use ely::{theme::Mode, theme::Theme, Assets};
use gpui::{px, size, App, AppContext, Bounds, QuitMode, WindowBounds, WindowOptions};
use gpui_platform::application;
use ld2_core::config::ConfigStore;
use ld2_core::l10n::Lang;
use ld2_core::store::AppStore;
use ld2_core::transport::{self, TransportConfig};
use tokio::sync::mpsc;

use crate::root::{Root, Shell};

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

    // M11.5: тема/шрифт/масштаб Ely ↔ config (ФР-19, §13, НФР min 12px).
    let theme_light = config.theme == "light";
    let font_scale = config.font_scale;

    let shell = Shell {
        store: app_store,
        config,
        config_store: store,
        lang,
        transport,
        ingress: rx,
        update_rx,
    };

    // D16/M11.1: gpui-pre 0.3.10 + ассеты Ely (шрифты Inter/JetBrains Mono,
    // Lucide-иконки, PLEX для svg-рендерера) — `init` грузит их через AssetSource.
    application()
        .with_assets(Assets)
        // macOS по умолчанию QuitMode::Explicit — закрытие окна не
        // завершало процесс (баг M11); LastWindowClosed — на всех ОС.
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            // Ely: шрифты+тема+Tab-биндинги — «call once, first» (README).
            ely::init(cx).expect("Ely init");
            // Тема из config (ФР-19): без фейда при старте.
            Theme::set_mode_now(if theme_light { Mode::Light } else { Mode::Dark }, cx);
            // §13: PT Sans — основной шрифт Ely-компонентов (кириллица embedded);
            // масштаб — НФР «размер шрифта» (min 12px применяет сам Ely).
            {
                let theme = cx.global_mut::<Theme>();
                theme.font_family = "PT Sans".into();
                theme.font_scale = font_scale;
            }
            cx.refresh_windows();

            // §13: embedded-шрифт с кириллицей (PT Sans, OFL) — без системных.
            let fonts = [
                Cow::Borrowed(include_bytes!("../assets/fonts/PT_Sans-Web-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/PT_Sans-Web-Bold.ttf").as_slice()),
            ];
            if let Err(e) = cx.text_system().add_fonts(fonts.into()) {
                tracing::warn!("не удалось загрузить шрифты: {e:#}");
            }

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

            // M14/F21: смена раскладки клавиатуры → перерисовка экрана
            // входа (предупреждение читает раскладку в рендере).
            let layout_watch = root.clone();
            cx.on_keyboard_layout_change(move |acx| {
                layout_watch.update(acx, |_root, cx| cx.notify());
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

            // M16/F29e: окно приложения — 90% монитора по центру
            // (было фиксированные 960×640).
            let win_size = cx
                .primary_display()
                .map(|d| {
                    let b = d.visible_bounds();
                    size(b.size.width * 0.9, b.size.height * 0.9)
                })
                .unwrap_or(size(px(960.0), px(640.0)));
            let bounds = Bounds::centered(None, win_size, cx);
            cx.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Likedimion".into()),
                        // M14/F15: системный тайтлбар скрыт — рисуем
                        // свой (название + ⚙ + WindowControl ✕).
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    // M14/F15: приложение само перетаскивает окно
                    // (Window::start_window_move из drag-региона).
                    app_owns_titlebar_drag: true,
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // M14/F2: минимум окна — 960×640 (ТЗ: компоновка
                    // трёх колонок не должна схлопываться).
                    window_min_size: Some(size(px(960.0), px(640.0))),
                    ..Default::default()
                },
                move |_, _| root.clone(),
            )
            .expect("окно");
            cx.activate(true);
        });
    Ok(())
}

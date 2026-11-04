//! # ld2-client
//!
//! Desktop-клиент текстовой онлайн-MMORPG ld2 (**eframe/egui** — первый UI).
//!
//! Ядро (store/transport/l10n/config/secrets/notify/update_check) живёт в
//! `ld2-core` (D12, PLAN-M10) и экспортируется здесь re-export'ами — пути
//! `ld2_client::store::…` (UI, e2e-тесты) не меняются. Второй UI — gpui:
//! `ld2-client-gpui`.
//!
//! Структура (ТЗ §6.2):
//! - `ld2_core::transport` — WebSocket, `seq`/ack, reconnect/resync (M1);
//! - `ld2_core::store` — AppStore + редукторы (собирается из событий);
//! - [`app`] — egui-шелл; игровые панели — [`ui`] по ФР ТЗ §7;
//! - `ld2_core::config` / `l10n` / `secrets` — настройки, ru/en, keyring.

pub use ld2_core::{config, l10n, notify, secrets, store, transport, update_check};

pub mod app;
pub mod ui;

/// Точка входа приложения (вызывается бинарником `ld2-client`).
pub fn run() -> anyhow::Result<()> {
    let _log_guard = init_logging()?;
    tracing::info!("ld2 client v{} — старт", env!("CARGO_PKG_VERSION"));

    let store = config::ConfigStore::open()?;
    let cfg = store.load()?;
    tracing::info!("конфиг: host={}, lang={}", cfg.host, cfg.lang);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "ld2",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg, store)))),
    )?;
    Ok(())
}

/// Rolling-лог в `<data_dir>/logs/ld2.log` (ТЗ НФР: логи без секретов).
/// Возвращает guard — держать живым всё время работы приложения.
fn init_logging() -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    let dirs = config::project_dirs()?;
    let log_dir = dirs.data_dir().join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, "ld2.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(non_blocking)
        .init();
    Ok(guard)
}

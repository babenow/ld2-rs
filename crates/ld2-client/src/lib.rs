//! # ld2-client
//!
//! Desktop-клиент текстовой онлайн-MMORPG ld2 (eframe/egui).
//!
//! Структура (ТЗ §6.2):
//! - [`transport`] — WebSocket, `seq`/ack, reconnect/resync (M1, PLAN-M1 T2);
//! - [`store`] — AppStore + редукторы (M1–M2: собирается только из событий);
//! - [`app`] — egui-шелл; игровые панели — с M2 по ФР ТЗ §7;
//! - [`config`], [`l10n`], [`secrets`] — локальные настройки, ru/en, keyring.

pub mod app;
pub mod config;
pub mod l10n;
pub mod notify;
pub mod secrets;
pub mod store;
pub mod transport;
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

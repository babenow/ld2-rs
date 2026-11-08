//! Rolling-лог приложения (ТЗ НФР: логи без секретов).
//!
//! Общий для обоих клиентов (egui/gpui, D12): `<data_dir>/logs/ld2.log`,
//! daily-ротация, фильтр `RUST_LOG` (по умолчанию `info`).

use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;

/// Инициализация логирования. Возвращает guard — держать живым всё время
/// работы приложения (иначе non-blocking-буфер не сбрасывается).
pub fn init() -> Result<WorkerGuard> {
    let dirs = crate::config::project_dirs()?;
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

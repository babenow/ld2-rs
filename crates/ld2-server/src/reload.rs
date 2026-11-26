//! M27 (hot reload контента, фаза 1): перечитать каталог `content/` на
//! работающем сервере, идемпотентно применить сид в PostgreSQL и атомарно
//! заменить in-memory части (диалоги/магазины/версия), сбросив кэш карты.
//!
//! Триггеры: poll по отпечатку каталога (`world.content_poll_s`), SIGHUP
//! (unix) и admin-команда `cmd/admin.content.reload`. Мир (мобы/трупы)
//! этой фазой **не** переселяется — только контент-данные; новые мобы в
//! новых локациях появятся при следующем рестарте/фазе 2.

use std::time::Duration;

use tracing::{info, warn};

use crate::content::{self, ContentError};
use crate::state::{ContentState, SharedState};

/// Сериализация перезагрузок: poll/SIGHUP/admin не должны применять сид
/// одновременно (конкурируют за одну и ту же версию контента).
static RELOAD_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Что применилось (для лога и ack админ-команды).
#[derive(Debug, Clone)]
pub struct ReloadOutcome {
    pub version: String,
    pub applied: bool,
    pub locations: usize,
    pub items: usize,
}

/// Перечитать контент и заменить его в работающем сервере. При ошибке
/// (плохой JSON/валидация/БД) текущее состояние и БД не меняются:
/// транзакция сида откатывается, `meta.content_version` остаётся прежней.
pub async fn reload_state(state: &SharedState) -> Result<ReloadOutcome, ContentError> {
    let _guard = RELOAD_LOCK.lock().await;
    let report = content::reload(&state.pool, &state.config.world.content_dir).await?;
    let previous = state.content().version.clone();
    // Подмена памяти — только после успешного сида: читатели видят либо
    // старое, либо полностью новое состояние.
    state.set_content(ContentState {
        version: report.version.clone(),
        dialogs: report.dialogs,
        shops: report.shops,
    });
    state.invalidate_map_cache().await;
    info!(
        "контент: hot reload {} → {} (БД: {}, локаций {}, предметов {})",
        short(&previous),
        short(&report.version),
        if report.applied {
            "применён"
        } else {
            "без изменений"
        },
        report.locations,
        report.items
    );
    Ok(ReloadOutcome {
        version: report.version,
        applied: report.applied,
        locations: report.locations,
        items: report.items,
    })
}

fn short(version: &str) -> &str {
    version.get(..12).unwrap_or(version)
}

async fn reload_and_log(state: &SharedState, trigger: &str) {
    match reload_state(state).await {
        Ok(outcome) if !outcome.applied => {
            info!("контент: {trigger} — версия не изменилась (сид не требовался)");
        }
        Ok(_) => {}
        Err(e) => warn!("контент: {trigger} — перезагрузка не удалась: {e}"),
    }
}

/// Запустить фоновые триггеры hot reload (poll + SIGHUP). Останавливаются
/// по graceful shutdown (`request_shutdown`).
pub fn spawn_watcher(state: SharedState) {
    spawn_poll_watcher(state.clone());
    #[cfg(unix)]
    {
        spawn_sighup_watcher(state);
    }
    #[cfg(not(unix))]
    {
        let _ = state;
    }
}

fn spawn_poll_watcher(state: SharedState) {
    let poll_s = state.config.world.content_poll_s;
    if poll_s == 0 {
        return;
    }
    tokio::spawn(async move {
        let dir = state.config.world.content_dir.clone();
        let mut stopped = state.shutdown_rx();
        let mut last = content::fingerprint(&dir);
        let mut ticker = tokio::time::interval(Duration::from_secs(poll_s));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Первый тик интервала — мгновенный: отпечаток только что снят,
        // повторная перезагрузка не нужна.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                _ = ticker.tick() => {
                    let now = content::fingerprint(&dir);
                    if now != last {
                        last = now;
                        reload_and_log(&state, "poll").await;
                    }
                }
            }
        }
        info!("контент: poll-watcher остановлен");
    });
}

#[cfg(unix)]
fn spawn_sighup_watcher(state: SharedState) {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async move {
        let mut hup = match signal(SignalKind::hangup()) {
            Ok(hup) => hup,
            Err(e) => {
                warn!("контент: SIGHUP недоступен: {e}");
                return;
            }
        };
        let mut stopped = state.shutdown_rx();
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                _ = hup.recv() => reload_and_log(&state, "SIGHUP").await,
            }
        }
        info!("контент: SIGHUP-watcher остановлен");
    });
}

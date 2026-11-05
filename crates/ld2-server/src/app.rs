//! HTTP-приложение: `/healthz` (M12.1); WS-транспорт — M12.2 (D23).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use sqlx::PgPool;

/// Маршруты сервера; пул общий на все соединения.
pub fn router(pool: PgPool) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(pool)
}

/// `200 ok` при живой БД, `503` иначе (для мониторинга/оркестраторов).
/// Кортеж-ответ axum: `(части…, тело)` — статус до тела.
async fn healthz(State(pool): State<PgPool>) -> Result<(StatusCode, &'static str), StatusCode> {
    crate::db::ping(&pool).await.map_err(|e| {
        tracing::warn!("healthz: ошибка БД: {e}");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok((StatusCode::OK, "ok\n"))
}

/// Accept-loop с graceful shutdown: SIGINT/SIGTERM → дожидаемся завершения
/// текущих запросов, затем выходим (флаш состояния — D27, фаза M12.2+).
pub async fn serve(listener: tokio::net::TcpListener, pool: PgPool) -> std::io::Result<()> {
    axum::serve(listener, router(pool))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("установлен обработчик SIGINT");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("установлен обработчик SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("получен сигнал остановки — завершаю работу");
}

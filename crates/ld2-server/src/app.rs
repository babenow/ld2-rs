//! HTTP/WS-приложение: `/healthz` и WS-апгрейд `/ws` (§1), graceful shutdown.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;

use crate::state::SharedState;

/// Маршруты сервера; состояние (конфиг + пул) общее на все соединения.
pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/.well-known/ld2.json", get(discovery_doc))
        .route("/ws", get(crate::transport::ws_handler))
        .with_state(state)
}

/// M19 (ADR-013): well-known discovery-документ (вне WS-контракта):
/// `address` из `[discovery]`, `version` — версия WS-контракта,
/// `min_client` — из `[server].min_client` (§5), если задан.
/// Секция не настроена →404 (discovery выключен).
async fn discovery_doc(
    State(state): State<SharedState>,
) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    let address = state
        .config
        .discovery
        .address
        .as_deref()
        .filter(|a| !a.is_empty())
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut doc = serde_json::json!({
        "address": address,
        "version": ld2_protocol::envelope::PROTOCOL_VERSION,
    });
    if let Some(min_client) = state.config.server.min_client.as_deref() {
        doc["min_client"] = serde_json::json!(min_client);
    }
    Ok(axum::Json(doc))
}

/// `200 ok` при живой БД, `503` иначе (для мониторинга/оркестраторов).
async fn healthz(
    State(state): State<SharedState>,
) -> Result<(StatusCode, &'static str), StatusCode> {
    crate::db::ping(&state.pool).await.map_err(|e| {
        tracing::warn!("healthz: ошибка БД: {e}");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok((StatusCode::OK, "ok\n"))
}

/// Accept-loop с graceful shutdown: SIGINT/SIGTERM → дожидаемся завершения
/// текущих запросов, затем выходим (флаш состояния — D27, фаза M12.3+).
///
/// `into_make_service_with_connect_info` — нужен `ConnectInfo<SocketAddr>`
/// в WS-обработчике (rate-limit auth по IP, D30).
pub async fn serve(listener: tokio::net::TcpListener, state: SharedState) -> std::io::Result<()> {
    let mut stopped = state.shutdown_rx();
    let router = router(state);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! {
            _ = shutdown_signal() => {},
            // Остановка по запросу (request_shutdown): тесты «рестарта» (M12.6).
            _ = stopped.changed() => {},
        }
    })
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

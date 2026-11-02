//! Мок-сервер ld2 — M0: WS-слушатель с эхо-ответами.
//!
//! Назначение (PLAN-M0.md Фаза 2, `docs/protocol.md` §5 ТЗ): давать клиенту
//! живой адрес `ws://127.0.0.1:24817/ws` до появления контракта M1.
//!
//! Объём M1 (ТЗ §5): скриптованные сценарии (`hello`, `snapshot`, `death`,
//! `crime`, `trade`, `chaos`), команды `cmd/*` c `ack`, события `evt/*`,
//! фикстуры навыков/профессий из `docs/skills.md`.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tracing_subscriber::EnvFilter;

const MOCK_ADDR: &str = "127.0.0.1:24817";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let listener = TcpListener::bind(MOCK_ADDR).await?;
    tracing::info!("мок-сервер слушает ws://{MOCK_ADDR}/ws (эхо-режим M0)");

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::info!("подключение: {peer}");
        tokio::spawn(async move {
            if let Err(e) = echo_connection(stream).await {
                tracing::warn!("соединение {peer} завершено с ошибкой: {e}");
            } else {
                tracing::info!("соединение {peer} закрыто");
            }
        });
    }
}

/// Эхо: любое входящее текстовое сообщение возвращается как есть (M0).
async fn echo_connection(stream: TcpStream) -> Result<()> {
    let mut ws = tokio_tungstenite::accept_async(stream).await?;
    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(text) => {
                tracing::debug!("эхо {} байт", text.len());
                ws.send(Message::Text(text)).await?;
            }
            Message::Ping(payload) => ws.send(Message::Pong(payload)).await?,
            Message::Close(_) => break,
            Message::Binary(_) => {
                // protocol.md §1: бинарные фреймы = ошибка протокола.
                tracing::warn!("бинарный фрейм — закрываю соединение");
                ws.close(None).await?;
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

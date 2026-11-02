//! WS-сервер мока: accept-loop + обработка соединения через [`Session`].

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::chaos::Chaos;
use crate::session::{Out, Session};

#[derive(Clone)]
pub struct MockOptions {
    /// Например `127.0.0.1:24817`.
    pub bind: String,
    pub chaos: Chaos,
    /// Подпись в логе/помощи.
    pub scenario: String,
}

/// Основной цикл: слушаем `opts.bind` вечно.
pub async fn serve(opts: MockOptions) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&opts.bind).await?;
    let addr = listener.local_addr()?;
    info!(
        "мок-сервер слушает ws://{addr} (сценарий: {}, chaos: {})",
        opts.scenario,
        if opts.scenario == "chaos" {
            "вкл"
        } else {
            "выкл"
        }
    );
    serve_with_listener(listener, opts.chaos).await
}

/// Вариант для тестов: свой listener (эфемерный порт), свой chaos.
pub async fn serve_with_listener(listener: TcpListener, chaos: Chaos) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let chaos = chaos.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, peer, chaos).await {
                warn!("соединение {peer} завершено: {e:#}");
            }
        });
    }
}

async fn handle(stream: TcpStream, peer: SocketAddr, mut chaos: Chaos) -> anyhow::Result<()> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut stream) = ws.split();
    let mut session = Session::new();
    debug_conn(peer);

    while let Some(msg) = stream.next().await {
        match msg? {
            Message::Text(text) => {
                for out in session.on_text(text.as_str()) {
                    match out {
                        Out::Text(t) => {
                            sink.send(Message::Text(t.into())).await?;
                        }
                        Out::Close => {
                            sink.close().await?;
                            return Ok(());
                        }
                    }
                }
                // §5 ТЗ: сценарий chaos — детерминированные случайные обрывы.
                if chaos.should_drop() {
                    warn!("chaos: обрыв соединения {peer} (seed-последовательность)");
                    sink.send(Message::Close(None)).await?;
                    return Ok(());
                }
            }
            Message::Ping(p) => sink.send(Message::Pong(p)).await?,
            Message::Close(_) => break,
            Message::Binary(_) => {
                // protocol.md §1: бинарные фреймы = ошибка протокола.
                warn!("бинарный фрейм от {peer} — закрываю");
                sink.close().await?;
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

fn debug_conn(peer: SocketAddr) {
    info!("подключение: {peer}");
}

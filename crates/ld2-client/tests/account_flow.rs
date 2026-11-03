//! Сквозной тест M2: реальный мок + реальный транспорт + [`AppStore`]
//! (без egui — редьюсер отделён от UI, ТЗ §6.2).
//!
//! **DoD M2:** «смена персонажа = чистый сброс сторов; лимит слотов меняется
//! конфигом мока без правки кода».

use std::time::Duration;

use ld2_client::store::{AppStore, Effect, Screen};
use ld2_client::transport::{self, IngressEvent, TransportConfig, TransportStatus};
use ld2_mock_server::{serve_configured, serve_with_listener, Chaos};
use ld2_protocol::cmd::{AuthLogin, CharSelect};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

async fn start_mock() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = serve_with_listener(listener, Chaos::disabled()).await;
    });
    format!("ws://{addr}")
}

async fn start_mock_with_slots(limit: u32) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = serve_configured(
            listener,
            Chaos::disabled(),
            ld2_mock_server::Knobs::active(),
            limit,
        )
        .await;
    });
    format!("ws://{addr}")
}

fn spawn_pair(
    url: &str,
) -> (
    transport::TransportHandle,
    mpsc::UnboundedReceiver<IngressEvent>,
    AppStore,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = transport::spawn(TransportConfig::new(url), tx);
    (handle, rx, AppStore::new())
}

/// Качать ingress в стор, пока не выполнится условие; возвращает накопленные
/// эффекты (их исполняет App в бою — здесь проверяем напрямую).
async fn pump<F: Fn(&AppStore) -> bool>(
    store: &mut AppStore,
    rx: &mut mpsc::UnboundedReceiver<IngressEvent>,
    pred: F,
) -> Vec<Effect> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut effects = Vec::new();
    loop {
        if pred(store) {
            return effects;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(ev)) => effects.extend(store.apply(ev)),
            Ok(None) => panic!("канал ingress закрыт"),
            Err(_) => panic!("таймаут флоу: screen={:?}", store.screen),
        }
    }
}

/// Полный аккаунт-флоу: вход → выбор → hot-switch → повторный вход → логаут.
#[tokio::test]
async fn account_flow_login_select_switch_logout() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);

    // Транспорт поднялся.
    pump(&mut store, &mut rx, |s| {
        s.transport == TransportStatus::Online
    })
    .await;

    // Вход по демо-аккаунту → session.start → Characters + эффекты keyring.
    let seq = handle
        .send(&AuthLogin {
            login: "demo".into(),
            password: "demo123".into(),
        })
        .expect("send login");
    store.on_login_sent("demo");
    let effects = pump(&mut store, &mut rx, |s| {
        s.screen == Screen::Characters && s.session.is_some()
    })
    .await;
    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::PersistSession { login, refresh_token }
                if login == "demo" && !refresh_token.is_empty())
        ),
        "токен сохраняется в keyring (ФР-01): {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::TransportToken(_))),
        "токен уходит в транспорт для resume (§4.5)"
    );
    let _ = seq;
    assert_eq!(store.slot_usage(), (1, 8));

    // Выбор персонажа → снапшот → Game.
    handle
        .send(&CharSelect {
            char_id: "c1".into(),
        })
        .expect("send select");
    store.on_select_sent();
    pump(&mut store, &mut rx, |s| s.screen == Screen::Game).await;
    let snap = store.snapshot.as_ref().expect("снимок");
    assert_eq!(snap.char.name, "Ким");

    // DoD M2: hot-switch — чистый сброс сторов БЕЗ ре-логина.
    store.back_to_characters();
    assert_eq!(store.screen, Screen::Characters);
    assert!(store.snapshot.is_none(), "снимок выгружен (чистый сброс)");
    assert!(store.session.is_some(), "сессия пережила переключение");

    // Повторный вход тем же токеном — снова Game.
    handle
        .send(&CharSelect {
            char_id: "c1".into(),
        })
        .expect("send select 2");
    store.on_select_sent();
    pump(&mut store, &mut rx, |s| s.screen == Screen::Game).await;
    assert_eq!(store.snapshot.as_ref().expect("снимок").char.name, "Ким");

    // Логаут: чистый сброс + отзыв токена.
    let effects = store.logout();
    assert_eq!(store.screen, Screen::Login);
    assert!(store.session.is_none() && store.snapshot.is_none());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ForgetSession { login } if login == "demo")),
        "токен убран из keyring: {effects:?}"
    );

    handle.close();
}

/// Ошибка входа показывается на форме (ФР-01).
#[tokio::test]
async fn bad_credentials_surface_on_login_form() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    pump(&mut store, &mut rx, |s| {
        s.transport == TransportStatus::Online
    })
    .await;

    handle
        .send(&AuthLogin {
            login: "demo".into(),
            password: "wrong".into(),
        })
        .expect("send login");
    store.on_login_sent("demo");
    pump(&mut store, &mut rx, |s| s.login_error.is_some()).await;
    assert_eq!(
        store.login_error.as_deref(),
        Some("неверный логин или пароль"),
        "текст сервера (§9) на форме"
    );
    assert_eq!(store.screen, Screen::Login);
    handle.close();
}

/// DoD M2: лимит слотов приходит из КОНФИГА мока (3), а не из кода клиента.
#[tokio::test]
async fn slot_limit_comes_from_server_config() {
    let url = start_mock_with_slots(3).await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    pump(&mut store, &mut rx, |s| {
        s.transport == TransportStatus::Online
    })
    .await;

    handle
        .send(&AuthLogin {
            login: "demo".into(),
            password: "demo123".into(),
        })
        .expect("send login");
    store.on_login_sent("demo");
    pump(&mut store, &mut rx, |s| s.session.is_some()).await;

    let (used, limit) = store.slot_usage();
    assert_eq!(limit, 3, "лимит из session.start мока, не из константы");
    assert_eq!(used, 1);
    assert_eq!(
        store.session.as_ref().unwrap().char_slot_limit,
        3,
        "клиент рендерит серверный лимит (ФР-02)"
    );
    handle.close();
}

//! Сквозные e2e: реальный транспорт `ld2-client` ↔ реальный `ld2-mock-server`
//! (PLAN-M1 T4).
//!
//! **DoD M1 (ТЗ §10):** «demo-аккаунт → снапшот → команды с ack; обрыв сети →
//! восстановление ≤ 30 с без ручных действий» — здесь проверяется целиком,
//! без тестовых заглушек.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ld2_client::transport::{self, IngressEvent, TransportConfig, TransportStatus};
use ld2_mock_server::{serve_with_knobs, Chaos, Knobs};
use ld2_protocol::cmd::{AuthLogin, CharSelect, LocationMove};
use ld2_protocol::Envelope;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(10);

async fn start_mock() -> (String, Arc<Knobs>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let knobs = Knobs::active();
    let k = Arc::clone(&knobs);
    tokio::spawn(async move {
        let _ = serve_with_knobs(listener, Chaos::disabled(), k).await;
    });
    (format!("ws://{addr}"), knobs)
}

fn spawn_transport(
    url: &str,
    tweak: impl FnOnce(&mut TransportConfig),
) -> (
    transport::TransportHandle,
    mpsc::UnboundedReceiver<IngressEvent>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut cfg = TransportConfig::new(url);
    tweak(&mut cfg);
    (transport::spawn(cfg, tx), rx)
}

async fn wait_for<F>(rx: &mut mpsc::UnboundedReceiver<IngressEvent>, mut pred: F) -> IngressEvent
where
    F: FnMut(&IngressEvent) -> bool,
{
    let fut = async {
        while let Some(ev) = rx.recv().await {
            eprintln!("E2E {ev:?}"); // видно в логе упавшего теста
            if pred(&ev) {
                return ev;
            }
        }
        panic!("канал ingress закрыт раньше ожидаемого события");
    };
    tokio::time::timeout(WAIT, fut)
        .await
        .expect("таймаут ожидания события от транспорта")
}

/// Дождаться события `ty` и вернуть его payload.
async fn wait_event(rx: &mut mpsc::UnboundedReceiver<IngressEvent>, ty: &str) -> serde_json::Value {
    let ev = wait_for(
        rx,
        |e| matches!(e, IngressEvent::Event(env) if env.msg_type == ty),
    )
    .await;
    match ev {
        IngressEvent::Event(env) => env.payload.unwrap_or_default(),
        other => panic!("ожидалось событие {ty}, получено: {other:?}"),
    }
}

async fn wait_online(rx: &mut mpsc::UnboundedReceiver<IngressEvent>) {
    wait_for(rx, |e| {
        matches!(e, IngressEvent::Status(TransportStatus::Online))
    })
    .await;
}

async fn wait_ack_ok(
    rx: &mut mpsc::UnboundedReceiver<IngressEvent>,
    seq: u32,
) -> ld2_protocol::Ack {
    let ev = wait_for(rx, |e| matches!(e, IngressEvent::Ack(a) if a.seq == seq)).await;
    match ev {
        IngressEvent::Ack(ack) => {
            assert!(ack.is_ok(), "ack seq={seq} должен быть ok: {:?}", ack.error);
            ack
        }
        other => panic!("ожидался ack, получено: {other:?}"),
    }
}

async fn wait_until<F: Fn() -> bool>(cond: F, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("условие не наступило за {timeout:?}");
}

/// DoD M1, первая половина: demo-аккаунт → снапшот → команды с ack.
#[tokio::test]
async fn full_stack_login_select_move() {
    let (url, _knobs) = start_mock().await;
    let (handle, mut rx) = spawn_transport(&url, |_| {});
    wait_online(&mut rx).await;

    // Вход по демо-аккаунту → session.start (§5) → refresh-токен в транспорт.
    let seq = handle
        .send(&AuthLogin {
            login: "demo".into(),
            password: "demo123".into(),
        })
        .expect("send login");
    wait_ack_ok(&mut rx, seq).await;
    let start = wait_event(&mut rx, "evt/session.start").await;
    assert_eq!(start["char_slot_limit"], 8, "D6: лимит из конфига сервера");
    let token = start["session"]["refresh"]["token"]
        .as_str()
        .expect("refresh token")
        .to_string();
    handle
        .set_refresh_token(Some(token))
        .expect("token в транспорт");

    // Выбор персонажа → полный снапшот (ТЗ §4.8.2).
    let seq = handle
        .send(&CharSelect {
            char_id: "c1".into(),
        })
        .expect("send select");
    let ack = wait_ack_ok(&mut rx, seq).await;
    assert!(ack.state_version.is_some(), "select меняет состояние");
    let snap = wait_event(&mut rx, "evt/state/snapshot").await;
    assert_eq!(snap["char"]["name"], "Ким");
    assert_eq!(snap["char"]["mp"]["cur"], 45, "D4: mp в снапшоте");
    assert_eq!(snap["char"]["xp_pool"], 120);
    assert_eq!(snap["location"]["id"], "l_village");

    // Команда с ack: переход в лес.
    let seq = handle
        .send(&LocationMove {
            exit_id: "e_north".into(),
        })
        .expect("send move");
    let ack = wait_ack_ok(&mut rx, seq).await;
    let payload = ack.payload.expect("payload перехода");
    assert_eq!(payload["location"]["id"], "l_forest");

    handle.close();
    wait_for(&mut rx, |e| {
        matches!(e, IngressEvent::Status(TransportStatus::Stopped))
    })
    .await;
}

/// DoD M1, вторая половина: обрыв → авто-восстановление ≤ 30 с без ручных
/// действий; на новом соединении `state.resync` несёт последнюю версию (§3).
#[tokio::test]
async fn full_stack_reconnect_within_30s() {
    let (url, knobs) = start_mock().await;
    // Соединение №1 рвётся после ТРЁХ записей bootstrap-фазы: resume,
    // служебный time.sync (M6) и resync (пишется после resume-ack) — в
    // недетерминированном порядке; бюджет 2 позволял drop'у съесть одну из
    // них до ack (флаки CI). Три слота = полная фаза, conn2 не рвётся.
    knobs
        .drop_first_conn_after
        .store(3, std::sync::atomic::Ordering::SeqCst);

    let (handle, mut rx) = spawn_transport(&url, |c| {
        c.refresh_token = Some("rt_e2e".into());
    });
    let t0 = Instant::now();

    // Соединение №1: resume → session.start → resync → обрыв.
    wait_online(&mut rx).await;
    let start1 = wait_event(&mut rx, "evt/session.start").await;
    assert_eq!(start1["server"]["name"], "ld2-dev");

    // Клиент замечает обрыв и уходит в backoff (0.5→30 с).
    wait_for(&mut rx, |e| {
        matches!(
            e,
            IngressEvent::Status(TransportStatus::Reconnecting { .. })
        )
    })
    .await;

    // Соединение №2: bootstrap resume → session.start повторно.
    wait_online(&mut rx).await;
    let _start2 = wait_event(&mut rx, "evt/session.start").await;

    let elapsed = t0.elapsed();
    assert!(
        elapsed <= Duration::from_secs(30),
        "восстановление заняло {elapsed:?} — DoD требует ≤ 30 с"
    );

    // Служебный time.sync (M6) занимает слот обрывающегося соединения —
    // resync первого соединения мог не дойти; проверяем то, что гарантируется:
    // каждый дошедший resync несёт НЕНУЛЕВУЮ версию из ack (регрессия фикса
    // M1) и последний — не меньше первой версии сессии (§3, монотонность).
    wait_until(
        || knobs.received_count("cmd/state.resync") >= 1,
        Duration::from_secs(5),
    )
    .await;
    let resyncs: Vec<Envelope> = knobs
        .received
        .lock()
        .expect("mutex")
        .iter()
        .filter(|e| e.msg_type == "cmd/state.resync")
        .cloned()
        .collect();
    assert!(
        !resyncs.is_empty(),
        "resync дошёл хотя бы на одном соединении"
    );
    let versions: Vec<u64> = resyncs
        .iter()
        .map(|e| {
            e.payload.as_ref().expect("payload")["last_state_version"]
                .as_u64()
                .expect("версия")
        })
        .collect();
    assert!(
        versions.iter().all(|v| *v > 0),
        "нулевая версия = регрессия фикса state_version: {versions:?}"
    );
    let last = *versions.iter().max().expect("непусто");
    assert!(
        last >= 1001,
        "последняя версия из ack resume (conn2): {last} < 1001"
    );

    handle.close();
}

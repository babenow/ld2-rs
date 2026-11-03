//! Интеграционные тесты транспорта против тестового сервера (PLAN-M1 T2).
//!
//! Ключевые — DoD M1: «подключение к моку, команда→ack, обрыв сети →
//! восстановление ≤ 30 с без ручных действий».

mod common;

use std::time::{Duration, Instant};

use ld2_client::transport::{self, IngressEvent, TransportConfig, TransportStatus};
use ld2_protocol::cmd::{LocationMove, Ping};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(10);

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

/// Ждать первое событие, удовлетворяющее предикату (остальные поглощаются).
async fn wait_for<F>(rx: &mut mpsc::UnboundedReceiver<IngressEvent>, mut pred: F) -> IngressEvent
where
    F: FnMut(&IngressEvent) -> bool,
{
    let fut = async {
        while let Some(ev) = rx.recv().await {
            eprintln!("WFR {ev:?}"); // видно в логе упавшего теста
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

async fn wait_online(rx: &mut mpsc::UnboundedReceiver<IngressEvent>) {
    wait_for(rx, |e| {
        matches!(e, IngressEvent::Status(TransportStatus::Online))
    })
    .await;
}

/// Поллинг условия на «ручках» сервера.
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

/// DoD M1: команда → ack с `state_version` от сервера.
#[tokio::test]
async fn command_gets_ack() {
    let (url, knobs) = common::spawn_mock().await;
    let (handle, mut rx) = spawn_transport(&url, |_| {});

    wait_online(&mut rx).await;
    let seq = handle.send(&Ping {}).expect("send ping");

    let ev = wait_for(
        &mut rx,
        |e| matches!(e, IngressEvent::Ack(a) if a.seq == seq),
    )
    .await;
    match ev {
        IngressEvent::Ack(ack) => {
            assert!(ack.is_ok(), "ping должен быть ok: {:?}", ack.error);
            assert_eq!(ack.state_version, Some(42), "state_version из ack (§2.2)");
        }
        other => panic!("ожидался ack, получено: {other:?}"),
    }
    assert_eq!(knobs.received_count("cmd/ping"), 1);

    handle.close();
    wait_for(&mut rx, |e| {
        matches!(e, IngressEvent::Status(TransportStatus::Stopped))
    })
    .await;
}

/// События `evt/*` проходят транспортом без изменений (§2.3).
#[tokio::test]
async fn server_events_are_forwarded() {
    let (url, knobs) = common::spawn_mock().await;
    knobs
        .send_pong_on_connect
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (handle, mut rx) = spawn_transport(&url, |_| {});
    wait_online(&mut rx).await;

    let ev = wait_for(
        &mut rx,
        |e| matches!(e, IngressEvent::Event(env) if env.msg_type == "evt/pong"),
    )
    .await;
    match ev {
        IngressEvent::Event(env) => {
            assert_eq!(env.v, 1);
            assert!(env.payload.is_some(), "payload события сохраняется");
        }
        other => panic!("ожидалось событие, получено: {other:?}"),
    }
    handle.close();
}

/// **DoD M1:** обрыв → авто-reconnect с `auth.resume` → `state.resync`
/// с последней известной версией, уложившись в 30 секунд.
#[tokio::test]
async fn reconnects_and_resyncs_within_30s() {
    let (url, knobs) = common::spawn_mock().await;
    // Обрыв на соединении №1 после ТРЁХ сообщений в ЛЮБОМ порядке:
    // bootstrap `auth.resume`, служебный `time.sync` (M6) и `cmd/ping`
    // могут прийти в разном порядке (select без biased) — бюджет 2 позволял
    // drop'у съесть ping до ack → флаки CI. Три слота покрывают любую
    // перестановку; conn2 не рвётся.
    knobs
        .drop_first_conn_after
        .store(3, std::sync::atomic::Ordering::SeqCst);

    let (handle, mut rx) = spawn_transport(&url, |c| {
        c.refresh_token = Some("rt-1".into());
    });

    wait_online(&mut rx).await;
    let seq = handle.send(&Ping {}).expect("send ping");
    wait_for(
        &mut rx,
        |e| matches!(e, IngressEvent::Ack(a) if a.seq == seq),
    )
    .await;
    let t0 = Instant::now(); // с этого момента соединение рвётся

    // Клиент замечает обрыв и уходит в backoff (0.5с → …, потолок 30с).
    let ev = wait_for(&mut rx, |e| {
        matches!(
            e,
            IngressEvent::Status(TransportStatus::Reconnecting { .. })
        )
    })
    .await;
    assert!(
        matches!(
            ev,
            IngressEvent::Status(TransportStatus::Reconnecting { .. })
        ),
        "ожидался Reconnecting: {ev:?}"
    );

    // Второе подключение.
    wait_for(&mut rx, |e| {
        matches!(e, IngressEvent::Status(TransportStatus::Online))
    })
    .await;

    // Bootstrap-цепочка на новом соединении: resume → resync.
    wait_until(
        || {
            knobs.received_count("cmd/auth.resume") >= 1
                && knobs.received_count("cmd/state.resync") >= 1
        },
        Duration::from_secs(25),
    )
    .await;

    let elapsed = t0.elapsed();
    assert!(
        elapsed <= Duration::from_secs(30),
        "восстановление заняло {elapsed:?} — DoD требует ≤ 30 с"
    );

    // resync обязан нести последнюю известную версию (из ack оборванной сессии).
    let resync = knobs.find("cmd/state.resync").expect("resync отправлен");
    assert_eq!(
        resync.payload.as_ref().expect("payload")["last_state_version"],
        42,
        "state_version отслеживается через ack (§3)"
    );

    handle.close();
}

/// Таймаут ack: идемпотентная команда ретраится ровно один раз, затем
/// `CommandTimeout`; неидемпотентная — без ретраев (§2.4).
#[tokio::test]
async fn ack_timeout_retry_policy() {
    let (url, knobs) = common::spawn_mock().await;
    knobs
        .respond
        .store(false, std::sync::atomic::Ordering::SeqCst); // молчим

    let (handle, mut rx) = spawn_transport(&url, |c| {
        c.ack_timeout = Duration::from_millis(400);
    });
    wait_online(&mut rx).await;

    // Идемпотентный ping: отправка → ретрай (2 отправки) → CommandTimeout.
    let seq = handle.send(&Ping {}).expect("send ping");
    wait_until(
        || knobs.received_count("cmd/ping") == 2,
        Duration::from_secs(5),
    )
    .await;
    wait_for(
        &mut rx,
        |e| matches!(e, IngressEvent::CommandTimeout { seq: s, .. } if *s == seq),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        knobs.received_count("cmd/ping"),
        2,
        "более одного ретрая быть не должно (§2.4)"
    );

    // Неидемпотентный переход: одна отправка, никаких повторов.
    let seq2 = handle
        .send(&LocationMove {
            exit_id: "e_north".into(),
        })
        .expect("send move");
    wait_for(
        &mut rx,
        |e| matches!(e, IngressEvent::CommandTimeout { seq: s, .. } if *s == seq2),
    )
    .await;
    assert_eq!(
        knobs.received_count("cmd/location.move"),
        1,
        "неидемпотентная команда не ретраится (§7)"
    );

    handle.close();
}

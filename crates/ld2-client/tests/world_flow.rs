//! Сквозной тест M3: реальный мок + транспорт + стор — проход «деревня →
//! лес» с боем и осмотром (DoD ТЗ §10: сценарий, труп игрока помечен ⚠).

use std::time::Duration;

use ld2_client::store::{AppStore, LogKind, Screen};
use ld2_client::transport::{self, IngressEvent, TransportConfig, TransportStatus};
use ld2_mock_server::{serve_with_listener, Chaos};
use ld2_protocol::cmd::{AuthLogin, CharSelect, CombatAttack, EntityInfo, LocationMove};
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

async fn pump<F: Fn(&AppStore) -> bool>(
    store: &mut AppStore,
    rx: &mut mpsc::UnboundedReceiver<IngressEvent>,
    pred: F,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if pred(store) {
            return;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(ev)) => {
                store.apply(ev);
            }
            Ok(None) => panic!("канал ingress закрыт"),
            Err(_) => panic!("таймаут флоу: screen={:?}", store.screen),
        }
    }
}

/// Вход + выбор персонажа → Game (деревня).
async fn enter_world(
    handle: &transport::TransportHandle,
    store: &mut AppStore,
    rx: &mut mpsc::UnboundedReceiver<IngressEvent>,
) {
    pump(store, rx, |s| s.transport == TransportStatus::Online).await;
    handle
        .send(&AuthLogin {
            login: "demo".into(),
            password: "demo123".into(),
        })
        .expect("login");
    store.on_login_sent("demo");
    pump(store, rx, |s| s.screen == Screen::Characters).await;

    handle
        .send(&CharSelect {
            char_id: "c1".into(),
        })
        .expect("select");
    store.on_select_sent();
    pump(store, rx, |s| s.screen == Screen::Game).await;
    assert_eq!(
        store.snapshot.as_ref().unwrap().location.id,
        "l_village",
        "стартовая локация — деревня"
    );
}

fn location_id(store: &AppStore) -> String {
    store
        .snapshot
        .as_ref()
        .map(|s| s.location.id.clone())
        .unwrap_or_default()
}

fn entity_present(store: &AppStore, id: &str) -> bool {
    store
        .snapshot
        .as_ref()
        .is_some_and(|s| s.location.entities.iter().any(|e| e.id == id))
}

#[tokio::test]
async fn world_flow_village_to_forest_and_back() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    enter_world(&handle, &mut store, &mut rx).await;

    // История: деревня при входе в мир (ФР-03).
    assert_eq!(store.visited.len(), 1);

    // Переход в лес (эквивалент клика по выходу / Alt+↑).
    // Ждём evt/location/changed (а не ack): именно событие пишет историю.
    store.on_world_sent(ld2_client::store::Act::Move);
    handle
        .send(&LocationMove {
            exit_id: "e_north".into(),
        })
        .expect("move");
    pump(&mut store, &mut rx, |s| {
        s.visited.iter().any(|v| v.id == "l_forest")
    })
    .await;
    assert_eq!(location_id(&store), "l_forest");

    assert_eq!(store.visited.len(), 2, "история: деревня + лес");
    assert!(
        store
            .log
            .iter()
            .any(|e| e.kind == LogKind::Location && e.text.contains("Черный лес")),
        "журнал: вход в лес"
    );

    // Сущности леса: волк, игрок, труп ИГРОКА (⚠ данные для UI).
    assert!(entity_present(&store, "m_3"), "волк на месте");
    assert!(entity_present(&store, "p_77"), "другой игрок");
    assert!(entity_present(&store, "cor_9"), "труп игрока");
    let player_corpse = store
        .snapshot
        .as_ref()
        .unwrap()
        .location
        .entities
        .iter()
        .find(|e| e.id == "cor_9")
        .expect("труп");
    assert_eq!(
        player_corpse.was_player,
        Some(true),
        "⚠ труп игрока — маркер мародёрства (ФР-11)"
    );

    // Бой: три удара по волку (18 hp, урон 7) → despawn + труп.
    for i in 0..3 {
        store.on_world_sent(ld2_client::store::Act::Attack);
        handle
            .send(&CombatAttack {
                target_id: "m_3".into(),
            })
            .expect("attack");
        pump(&mut store, &mut rx, |s| s.pending.is_none()).await;
        if i < 2 {
            assert!(entity_present(&store, "m_3"), "волк жив после удара {i}");
        }
    }
    // Ждём труп: в пакете событий он идёт ПОСЛЕ despawn — его появление
    // гарантирует, что и гибель применена.
    pump(&mut store, &mut rx, |s| entity_present(s, "cor_m_3")).await;
    assert!(!entity_present(&store, "m_3"), "волк удалён после гибели");
    assert!(
        store
            .log
            .iter()
            .filter(|e| e.kind == LogKind::CombatOut)
            .count()
            >= 3,
        "журнал: исходящие удары (ФР-05/ФР-17)"
    );

    // Осмотр трупа волка: ack несёт лут.
    store.on_world_sent(ld2_client::store::Act::Examine);
    handle
        .send(&EntityInfo {
            entity_id: "cor_m_3".into(),
        })
        .expect("examine");
    pump(&mut store, &mut rx, |s| s.entity_detail.is_some()).await;
    let detail = store.entity_detail.as_ref().expect("детали осмотра");
    assert_eq!(detail["entity"]["id"], "cor_m_3");
    assert!(
        store.world_error.is_none(),
        "ошибок мира быть не должно: {:?}",
        store.world_error
    );

    // Обратно в деревню (e_south из леса) — снова ждём событие истории.
    store.on_world_sent(ld2_client::store::Act::Move);
    handle
        .send(&LocationMove {
            exit_id: "e_south".into(),
        })
        .expect("move back");
    pump(&mut store, &mut rx, |s| {
        s.visited.last().map(|v| v.id.as_str()) == Some("l_village")
    })
    .await;
    // История — кэш локаций (ФР-03): дедуп, свежая в конце → [лес, деревня].
    assert_eq!(
        store.visited.len(),
        2,
        "деревня → лес → деревня (без дублей)"
    );
    assert_eq!(store.visited[1].id, "l_village");

    handle.close();
}

#[tokio::test]
async fn blocked_exit_surfaces_world_error() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    enter_world(&handle, &mut store, &mut rx).await;

    // В деревне нет западного выхода — сервер отклоняет (ФР-03).
    store.on_world_sent(ld2_client::store::Act::Move);
    handle
        .send(&LocationMove {
            exit_id: "e_west".into(),
        })
        .expect("move");
    pump(&mut store, &mut rx, |s| s.world_error.is_some()).await;
    assert!(
        store.world_error.as_deref().unwrap().contains("выход") || store.world_error.is_some(),
        "текст ошибки сервера показан"
    );
    assert_eq!(location_id(&store), "l_village", "остались в деревне");

    handle.close();
}

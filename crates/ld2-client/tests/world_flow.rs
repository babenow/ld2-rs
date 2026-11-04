//! Сквозной тест M3: реальный мок + транспорт + стор — проход «деревня →
//! лес» с боем и осмотром (DoD ТЗ §10: сценарий, труп игрока помечен ⚠).

use std::time::Duration;

use ld2_client::store::{AppStore, LogKind, Screen};
use ld2_client::transport::{self, IngressEvent, TransportConfig, TransportStatus};
use ld2_mock_server::{serve_with_listener, Chaos};
use ld2_protocol::cmd::{
    AuthLogin, CharSelect, CharXpInvest, ChatSend, CombatAttack, CraftStart, EntityInfo,
    LocationMove, MagicRuneCast, QuestTurnIn, UpgradePreview, UpgradeStart,
};
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
    enter_world_as(handle, store, rx, "demo", "c1").await;
}

/// Вход конкретным аккаунтом/персонажем (DoD M8 — второй клиент demo2/c2).
async fn enter_world_as(
    handle: &transport::TransportHandle,
    store: &mut AppStore,
    rx: &mut mpsc::UnboundedReceiver<IngressEvent>,
    login: &str,
    char_id: &str,
) {
    pump(store, rx, |s| s.transport == TransportStatus::Online).await;
    handle
        .send(&AuthLogin {
            login: login.into(),
            password: "demo123".into(),
        })
        .expect("login");
    store.on_login_sent(login);
    pump(store, rx, |s| s.screen == Screen::Characters).await;

    handle
        .send(&CharSelect {
            char_id: char_id.into(),
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

/// DoD M4 (ТЗ §10): бой → лут в инвентаре → смерть → ВСЁ потеряно →
/// респаун в деревне с полным hp → подтверждение закрывает модалку.
#[tokio::test]
async fn death_flow_loses_everything_and_respawns() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    enter_world(&handle, &mut store, &mut rx).await;

    // Переход в лес и убийство волка → лут в инвентарь (ФР-07).
    store.on_world_sent(ld2_client::store::Act::Move);
    handle
        .send(&LocationMove {
            exit_id: "e_north".into(),
        })
        .expect("move");
    pump(&mut store, &mut rx, |s| location_id(s) == "l_forest").await;

    for _ in 0..3 {
        store.on_world_sent(ld2_client::store::Act::Attack);
        handle
            .send(&CombatAttack {
                target_id: "m_3".into(),
            })
            .expect("attack");
        pump(&mut store, &mut rx, |s| s.pending.is_none()).await;
    }
    pump(&mut store, &mut rx, |s| {
        s.snapshot
            .as_ref()
            .is_some_and(|sn| sn.char.inventory.iter().any(|i| i.uid == "loot_m_3"))
    })
    .await;
    assert!(
        store
            .snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .any(|i| i.name.contains("Волк")),
        "трофея с волка в инвентаре"
    );

    // Смерть (сценарий мока через чат).
    handle
        .send(&ChatSend {
            channel: "location".into(),
            text: "/scenario death".into(),
            to: None,
        })
        .expect("death trigger");
    pump(&mut store, &mut rx, |s| s.death.is_some()).await;

    // Сводка утраченного — и там лут с волка, и стартовый набор (ФР-06).
    let lost: Vec<String> = store
        .death
        .as_ref()
        .unwrap()
        .lost_items
        .iter()
        .map(|i| i.name.clone())
        .collect();
    assert!(
        lost.iter().any(|n| n.contains("Волк")) && lost.iter().any(|n| n.contains("посох")),
        "утрачено всё: {lost:?}"
    );
    assert!(
        store.log.iter().any(|e| e.text.contains("ВЫ ПОГИБЛИ")),
        "смерть в журнале"
    );

    // Респаун: деревня, ресурсы полные, инвентарь пуст (применены removals/stats).
    pump(&mut store, &mut rx, |s| location_id(s) == "l_village").await;
    let snap = store.snapshot.as_ref().unwrap();
    assert!(
        snap.char.inventory.is_empty(),
        "инвентарь пуст после смерти"
    );
    assert_eq!(snap.char.hp.cur, snap.char.hp.max, "hp полный");
    assert!(
        snap.char.equip.values().flatten().next().is_none(),
        "экипировка снята"
    );
    assert!(store.death.is_some(), "модалка открыта до подтверждения");

    // Закрыть можно только подтверждением (ФР-06).
    store.confirm_death();
    assert!(store.death.is_none());
    assert_eq!(store.screen, Screen::Game, "мир снова доступен");
    assert_eq!(location_id(&store), "l_village");

    handle.close();
}

/// DoD M5 (ТЗ §10): «руна огня ×7 → 2 каста → +1 посох → провал улучшения
/// по правилу break».
#[tokio::test]
async fn magic_upgrade_flow_dod() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    enter_world(&handle, &mut store, &mut rx).await;

    let staff_charges = |s: &AppStore| -> i32 {
        s.snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .find(|i| i.uid == "i_17")
            .and_then(|i| i.sockets.as_ref())
            .and_then(|sockets| sockets.first())
            .and_then(|s| s.rune.as_ref())
            .map(|r| r.charges)
            .unwrap_or(-1)
    };
    let staff_upgrade = |s: &AppStore| -> i32 {
        s.snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .find(|i| i.uid == "i_17")
            .map(|i| i.upgrade)
            .unwrap_or(-1)
    };

    // Руна ×7: два каста → заряды 7→5, мана 45→25 (D4: мана И заряд).
    assert_eq!(staff_charges(&store), 7, "стартовое состояние");
    assert_eq!(store.snapshot.as_ref().unwrap().char.mp.cur, 45);
    for (expect_charges, expect_mp) in [(6, 35), (5, 25)] {
        store.on_world_sent(ld2_client::store::Act::Cast);
        handle
            .send(&MagicRuneCast {
                staff_uid: "i_17".into(),
                socket_index: 0,
                target_id: None,
            })
            .expect("каст руны");
        pump(&mut store, &mut rx, |s| {
            staff_charges(s) == expect_charges
                && s.snapshot.as_ref().unwrap().char.mp.cur == expect_mp
        })
        .await;
    }
    assert!(
        store
            .log
            .iter()
            .any(|e| e.kind == ld2_client::store::LogKind::Magic),
        "касты в журнале"
    );

    // +1 посох: превью → успех → уровень 2→3.
    store.open_upgrade("i_17".into(), "Ясеневый посох".into());
    handle
        .send(&UpgradePreview {
            item_uid: "i_17".into(),
        })
        .expect("превью");
    store.on_sheet_sent(ld2_client::store::Act::Preview);
    pump(&mut store, &mut rx, |s| {
        s.upgrade.as_ref().is_some_and(|v| v.preview.is_some())
    })
    .await;
    let preview_id = store.upgrade.as_ref().unwrap().preview.as_ref().unwrap()["preview_id"]
        .as_str()
        .unwrap()
        .to_string();

    store.on_sheet_sent(ld2_client::store::Act::Upgrade);
    handle
        .send(&UpgradeStart {
            item_uid: "i_17".into(),
            preview_id,
        })
        .expect("улучшение");
    pump(&mut store, &mut rx, |s| {
        s.upgrade.is_none() && staff_upgrade(s) == 3
    })
    .await;
    assert!(
        store
            .log
            .iter()
            .any(|e| e.kind == ld2_client::store::LogKind::Loot && e.text.contains("+3")),
        "успешное улучшение в журнале"
    );

    // Провал по break_on_fail: сценарий → превью → старт → предмет разрушен.
    handle
        .send(&ChatSend {
            channel: "location".into(),
            text: "/scenario upgrade_fail".into(),
            to: None,
        })
        .expect("сценарий");
    pump(&mut store, &mut rx, |s| {
        s.log.iter().any(|e| e.text.contains("провалится"))
    })
    .await;

    store.open_upgrade("i_17".into(), "Ясеневый посох".into());
    handle
        .send(&UpgradePreview {
            item_uid: "i_17".into(),
        })
        .expect("превью 2");
    store.on_sheet_sent(ld2_client::store::Act::Preview);
    pump(&mut store, &mut rx, |s| {
        s.upgrade.as_ref().is_some_and(|v| v.preview.is_some())
    })
    .await;
    let preview_id = store.upgrade.as_ref().unwrap().preview.as_ref().unwrap()["preview_id"]
        .as_str()
        .unwrap()
        .to_string();

    store.on_sheet_sent(ld2_client::store::Act::Upgrade);
    handle
        .send(&UpgradeStart {
            item_uid: "i_17".into(),
            preview_id,
        })
        .expect("улучшение 2");
    pump(&mut store, &mut rx, |s| {
        s.upgrade.is_none()
            && !s
                .snapshot
                .as_ref()
                .unwrap()
                .char
                .inventory
                .iter()
                .any(|i| i.uid == "i_17")
    })
    .await;
    assert!(
        store
            .log
            .iter()
            .any(|e| e.kind == ld2_client::store::LogKind::Crime && e.text.contains("РАЗРУШЕН")),
        "провал по break в журнале"
    );

    handle.close();
}

/// DoD M6 (ТЗ §10): «формула §4.10 проверена на моке; UI показывает blend
/// без пересчёта» — сервер считает вклад, клиент только хранит/отсчитывает.
#[tokio::test]
async fn crime_statuses_flow_dod() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    enter_world(&handle, &mut store, &mut rx).await;

    store.on_world_sent(ld2_client::store::Act::Move);
    handle
        .send(&LocationMove {
            exit_id: "e_north".into(),
        })
        .expect("move");
    pump(&mut store, &mut rx, |s| location_id(s) == "l_forest").await;

    // Осмотр трупа игрока → подтверждение лута (как это делает UI) → сбор.
    store.on_world_sent(ld2_client::store::Act::Examine);
    handle
        .send(&EntityInfo {
            entity_id: "cor_9".into(),
        })
        .expect("осмотр");
    pump(&mut store, &mut rx, |s| s.entity_detail.is_some()).await;
    let detail = store.entity_detail.clone().expect("детали");
    assert_eq!(detail["was_player"], true);
    let loot_uid = detail["loot"][0]["uid"].as_str().unwrap().to_string();
    let loot_name = detail["loot"][0]["name"].as_str().unwrap().to_string();

    store.set_loot_confirm(loot_uid.clone(), loot_name);
    store.clear_loot_confirm(); // подтверждение в окне
    handle
        .send(&ld2_protocol::cmd::ItemUse {
            item_uid: loot_uid.clone(),
            target_id: None,
        })
        .expect("забор лута");
    // Статус идёт ПОСЛЕДНИМ в пачке событий (item/added → corpse/looted →
    // crime/added → crime/status) — ждём именно его.
    pump(&mut store, &mut rx, |s| {
        s.snapshot
            .as_ref()
            .is_some_and(|sn| sn.char.crime.statuses.iter().any(|c| c.kind == "marauder"))
    })
    .await;
    assert!(
        store
            .snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .any(|i| i.uid == loot_uid),
        "лут в инвентаре"
    );

    // 1) Статус «Мародёр» назначен сервером (blend, а не полная сумма).
    let statuses = &store.snapshot.as_ref().unwrap().char.crime.statuses;
    let marauder = statuses
        .iter()
        .find(|s| s.kind == "marauder")
        .expect("мараудёр назначен");
    assert!(
        statuses.iter().any(|s| s.kind == "murderer"),
        "был и остаётся"
    );

    // 2) Формула §4.10: журнал несёт серверный вклад k·t·(1 − T/T_MAX).
    let entry = store
        .crime_log
        .iter()
        .find(|e| e.kind == "marauder")
        .expect("запись в журнале");
    let before = entry.total_s - entry.delta_s;
    let expected = (0.5_f64 * 7200.0 * (1.0 - before as f64 / 604_800.0)).round() as i64;
    assert!(
        (entry.delta_s - expected).abs() <= 1,
        "вклад {} ≠ формуле {} (T={before})",
        entry.delta_s,
        expected
    );
    assert!(
        entry.delta_s < 7200,
        "частичное сложение: {} < 7200",
        entry.delta_s
    );

    // 3) Клиент НЕ пересчитывает: срок равен серверному, отсчёт — только
    // разница now/offset (живой статус виден фильтру active).
    let now = ld2_client::store::local_epoch() + store.time_offset;
    assert!(
        marauder.expires_at > now,
        "серверный срок в будущем и не изменён клиентом"
    );
    let active = ld2_client::store::filter_active_statuses(statuses, now);
    assert!(
        active.iter().any(|s| s.kind == "marauder"),
        "бейдж виден в active-фильтре"
    );

    handle.close();
}

/// DoD M7 (ТЗ §10): «сквозной прогон: квест → XP → навык → крафт».
#[tokio::test]
async fn quest_craft_flow_dod() {
    let url = start_mock().await;
    let (handle, mut rx, mut store) = spawn_pair(&url);
    enter_world(&handle, &mut store, &mut rx).await;

    // 1) Квест «Волки» 4/5 → убийство → «к сдаче».
    store.on_world_sent(ld2_client::store::Act::Move);
    handle
        .send(&LocationMove {
            exit_id: "e_north".into(),
        })
        .expect("move");
    pump(&mut store, &mut rx, |s| location_id(s) == "l_forest").await;

    for _ in 0..3 {
        store.on_world_sent(ld2_client::store::Act::Attack);
        handle
            .send(&CombatAttack {
                target_id: "m_3".into(),
            })
            .expect("attack");
        pump(&mut store, &mut rx, |s| s.pending.is_none()).await;
    }
    pump(&mut store, &mut rx, |s| {
        s.snapshot.as_ref().is_some_and(|sn| {
            sn.char.quests.iter().any(|q| {
                q.id == "q_wolves" && matches!(q.state, ld2_protocol::types::QuestPhase::TurnIn)
            })
        })
    })
    .await;

    // 2) Сдача → пул 120 + 150 = 270.
    handle
        .send(&QuestTurnIn {
            quest_id: "q_wolves".into(),
        })
        .expect("сдача");
    pump(&mut store, &mut rx, |s| {
        s.snapshot.as_ref().is_some_and(|sn| sn.char.xp_pool == 270)
    })
    .await;

    // 3) Опыт → навык (вложение из пула).
    handle
        .send(&CharXpInvest {
            skill_id: "sword".into(),
            points: 100,
        })
        .expect("вложение");
    pump(&mut store, &mut rx, |s| {
        s.snapshot.as_ref().is_some_and(|sn| {
            sn.char.xp_pool == 170
                && sn
                    .char
                    .skills
                    .iter()
                    .any(|sk| sk.id == "sword" && sk.rank == 6)
        })
    })
    .await;

    // 4) Крафт меча из инвентарных ингредиентов (ФР-08).
    handle
        .send(&CraftStart {
            recipe_id: "rec_sword_iron".into(),
            qty: 1,
        })
        .expect("крафт");
    pump(&mut store, &mut rx, |s| {
        s.snapshot.as_ref().is_some_and(|sn| {
            sn.char
                .inventory
                .iter()
                .any(|i| i.item_type == "sword_iron")
        })
    })
    .await;
    assert!(
        store.log.iter().any(|e| e.text.contains("Крафт готов")),
        "крафт в журнале (ФР-17)"
    );

    handle.close();
}

/// DoD M8 (ТЗ §10): «2 клиента против мока: party + сделка + whisper».
#[tokio::test]
async fn social_flow_dod_two_clients() {
    let url = start_mock().await;
    let (ha, mut rxa, mut sa) = spawn_pair(&url);
    let (hb, mut rxb, mut sb) = spawn_pair(&url);
    // A — demo/c1 «Ким», B — demo2/c2 «Астра» (два аккаунта, DoD M8).
    enter_world_as(&ha, &mut sa, &mut rxa, "demo", "c1").await;
    enter_world_as(&hb, &mut sb, &mut rxb, "demo2", "c2").await;

    // ── 1) Whisper A → B (ФР-13): доставка адресату ──
    ha.send(&ChatSend {
        channel: "whisper".into(),
        text: "привет, Астра!".into(),
        to: Some("Астра".into()),
    })
    .expect("whisper");
    sa.on_sheet_sent(ld2_client::store::Act::Chat);
    pump(&mut sa, &mut rxa, |s| {
        s.chat.get("whisper").is_some_and(|h| !h.is_empty())
    })
    .await;
    pump(&mut sb, &mut rxb, |s| {
        s.chat.get("whisper").is_some_and(|h| !h.is_empty())
    })
    .await;
    assert_eq!(
        sb.chat.get("whisper").unwrap()[0].text,
        "привет, Астра!",
        "B получил whisper"
    );
    assert_eq!(
        sb.chat.get("whisper").unwrap()[0].from,
        "c1",
        "автор — клиент A"
    );

    // ── 2) Группа (ФР-14): create → invite → accept, оба видят состав ──
    ha.send(&ld2_protocol::cmd::PartyCreate {}).expect("create");
    sa.on_sheet_sent(ld2_client::store::Act::Party);
    pump(&mut sa, &mut rxa, |s| s.party.is_some()).await;

    ha.send(&ld2_protocol::cmd::PartyInvite {
        player_id: "c2".into(),
    })
    .expect("invite");
    sa.on_sheet_sent(ld2_client::store::Act::Party);
    pump(&mut sb, &mut rxb, |s| s.party_invite.is_some()).await;
    let invite_id = sb.party_invite.as_ref().unwrap().invite_id.clone();
    assert_eq!(sb.party_invite.as_ref().unwrap().from_name, "Ким");

    hb.send(&ld2_protocol::cmd::PartyAccept { invite_id })
        .expect("accept");
    sb.on_sheet_sent(ld2_client::store::Act::Party);
    pump(&mut sb, &mut rxb, |s| {
        s.party.as_ref().is_some_and(|p| p.members.len() == 2)
    })
    .await;
    pump(&mut sa, &mut rxa, |s| {
        s.party.as_ref().is_some_and(|p| p.members.len() == 2)
    })
    .await;
    let members: Vec<&str> = sa
        .party
        .as_ref()
        .unwrap()
        .members
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(members, vec!["c1", "c2"], "оба клиента видят состав");

    // ── 3) Сделка (ФР-15): открытие, зеркало, версии, обмен ──
    ha.send(&ld2_protocol::cmd::TradeOpen {
        player_id: "c2".into(),
    })
    .expect("open");
    sa.on_sheet_sent(ld2_client::store::Act::Trade);
    pump(&mut sa, &mut rxa, |s| s.trade.is_some()).await;
    pump(&mut sb, &mut rxb, |s| s.trade.is_some()).await;
    let offer_id = sa.trade.as_ref().unwrap().offer_id.clone();

    // A кладёт слиток m_1 → у B он в `them` (зеркало с сервера).
    ha.send(&ld2_protocol::cmd::TradeAddItem {
        offer_id: offer_id.clone(),
        item_uid: "m_1".into(),
        qty: None,
    })
    .expect("add_item");
    sa.on_sheet_sent(ld2_client::store::Act::Trade);
    pump(&mut sa, &mut rxa, |s| {
        s.trade.as_ref().is_some_and(|t| t.you.len() == 1)
    })
    .await;
    pump(&mut sb, &mut rxb, |s| {
        s.trade.as_ref().is_some_and(|t| t.them.len() == 1)
    })
    .await;
    assert_eq!(sb.trade.as_ref().unwrap().them[0].uid, "m_1");

    // Обе стороны фиксируют (сверка версии — на сервере, §9).
    let ver_a = sa.trade.as_ref().unwrap().version;
    ha.send(&ld2_protocol::cmd::TradePropose {
        offer_id: offer_id.clone(),
        version: ver_a,
    })
    .expect("propose");
    sa.on_sheet_sent(ld2_client::store::Act::Trade);
    pump(&mut sa, &mut rxa, |s| {
        s.trade.as_ref().is_some_and(|t| t.you_locked)
    })
    .await;
    // Ждём, пока фиксация A дойдёт зеркалом до B (иначе E_TRADE_STALE).
    pump(&mut sb, &mut rxb, |s| {
        s.trade.as_ref().is_some_and(|t| t.them_locked)
    })
    .await;
    let ver_b = sb.trade.as_ref().unwrap().version;
    hb.send(&ld2_protocol::cmd::TradeAccept {
        offer_id: offer_id.clone(),
        version: ver_b,
    })
    .expect("accept");
    sb.on_sheet_sent(ld2_client::store::Act::Trade);

    // Сделка закрыта обеим, предмет переехал: B получил m_1, A — потерял.
    pump(&mut sb, &mut rxb, |s| s.trade.is_none()).await;
    pump(&mut sa, &mut rxa, |s| s.trade.is_none()).await;
    pump(&mut sb, &mut rxb, |s| {
        s.snapshot
            .as_ref()
            .is_some_and(|sn| sn.char.inventory.iter().any(|i| i.uid == "m_1"))
    })
    .await;
    // Событие removed у A ещё в канале — читаем до ассерта.
    pump(&mut sa, &mut rxa, |s| {
        s.snapshot
            .as_ref()
            .is_some_and(|sn| sn.char.inventory.iter().all(|i| i.uid != "m_1"))
    })
    .await;
    assert!(
        sa.snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .all(|i| i.uid != "m_1"),
        "A отдал слиток"
    );
    assert!(
        sa.log.iter().any(|e| e.text.contains("состоялась")),
        "журнал сделки (ФР-17)"
    );

    ha.close();
    hb.close();
}

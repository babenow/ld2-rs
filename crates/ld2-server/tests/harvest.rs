//! M22: снятие шкур против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): гейты (профессия/нож), сервер-джоб5 с,
//! лут — шкуры/обрезки в общий список и мясо/потроха в блок «Внутри»,
//! XP только за снятие шкуры, повтор запрещён.

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::json;

/// Выдать предмет dev-сценарием → uid.
async fn give(c: &mut ld2_test_kit::WsClient, def_id: &str) -> String {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario give {def_id}") }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "give {def_id}: {ack}");
    let added = c.wait_event("evt/item/added").await;
    added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string()
}

/// Начислить золото.
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
}

/// Старт разделки → ack.
async fn harvest(c: &mut ld2_test_kit::WsClient, corpse: &str, action: &str) -> serde_json::Value {
    let seq = c
        .send(
            "cmd/corpse.harvest",
            json!({ "entity_id": corpse, "action": action }),
        )
        .await;
    c.ack(seq).await
}

/// Итог разделки (сервер-джоб5 с — ждём дольше лимита wait в5 с).
async fn harvest_result(c: &mut ld2_test_kit::WsClient) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let v = c.recv_json().await;
            if v["type"] == "evt/harvest/result" {
                return v;
            }
        }
    })
    .await
    .expect("evt/harvest/result")
}

/// Убить волка в лесу → id трупа.
async fn kill_wolf(c: &mut ld2_test_kit::WsClient) -> String {
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");
    let corpse = c.wait_event("evt/corpse/created").await;
    let id = corpse["payload"]["entity"]["id"]
        .as_str()
        .expect("id трупа")
        .to_string();
    // Бой окончен (цель уничтожена) — не мешает дальнейшим wait.
    c.wait_event("evt/combat/end").await;
    id
}

/// M22: полный флоу — гейты (нет профессии → нет ножа) → освежевание
/// (блок «Внутри») → снятие шкуры (XP/уровень) → повтор запрещён.
#[tokio::test]
async fn harvest_gates_gut_and_skin_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    let _ = char_id;

    let corpse = kill_wolf(&mut c).await;

    // Гейт1: профессия не изучена.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["error"]["code"], "E_PROFESSION", "без профессии: {ack}");

    // Обучение — у тренера в деревне (труп ждёт: TTL60 с).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в деревню");
    give_gold(&mut c, 1000).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn skinning");
    c.wait_event("evt/craft/profession").await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "назад в лес");

    // Гейт2: ножа нет.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["error"]["code"], "E_TOOL_MISSING", "без ножа: {ack}");

    give(&mut c, "skinning_knife").await;

    // Освежевание: старт → итог через5 с → блок «Внутри».
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["result"], "ok", "старт gut: {ack}");
    assert_eq!(ack["payload"]["action"], "gut");
    assert_eq!(ack["payload"]["name"], "Волк", "имя без «Труп:»");
    // Пока джоб идёт — второй старт запрещён.
    let ack2 = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(
        ack2["error"]["code"], "E_COOLDOWN",
        "повтор во время джоба: {ack2}"
    );

    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], true, "gut готов: {result}");
    assert_eq!(result["payload"]["action"], "gut");
    let info = {
        let seq = c
            .send("cmd/entity.info", json!({ "entity_id": corpse }))
            .await;
        c.ack(seq).await
    };
    let inner = info["payload"]["inner"].as_array().expect("inner");
    assert_eq!(inner.len(), 1, "мясо/потроха отдельно: {info}");
    let inner_def = inner[0]["def_id"].as_str().unwrap_or_default();
    assert!(
        inner_def == "meat" || inner_def == "offal",
        "нутри: {inner_def}"
    );
    // Повторное освежевание запрещено.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "повтор gut: {ack}");

    // Снятие шкуры: старт → итог + XP. M39: волк ур.20, проф.0 →
    // «шанс сложности» 25 % → 36 XP; до порога 40 уровня нет.
    let ack = harvest(&mut c, &corpse, "skin").await;
    assert_eq!(ack["result"], "ok", "старт skin: {ack}");
    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], true, "skin готов: {result}");
    let prof = c.wait_event("evt/craft/profession").await;
    assert_eq!(prof["payload"]["profession"]["id"], "skinning");
    assert_eq!(prof["payload"]["gain"], 36, "XP снятия шкуры: {prof}");
    let level = prof["payload"]["profession"]["level"].as_i64().unwrap_or(0);
    assert_eq!(level, 0, "36 < 40 — уровня нет: {prof}");

    // Лут: дроп волка (2) + шкура/обрезки (1…3 с критом) — общий
    // список; «Внутри» не тронут.
    let info = {
        let seq = c
            .send("cmd/entity.info", json!({ "entity_id": corpse }))
            .await;
        c.ack(seq).await
    };
    let loot = info["payload"]["loot"].as_array().expect("loot");
    assert!(
        (3..=5).contains(&loot.len()),
        "дроп2 + шкура/обрезки (+крит×2): {loot:?}"
    );
    let tier = loot
        .iter()
        .skip(2)
        .map(|i| i["def_id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    // Ранг персонажа1 → только обычное качество q1 (фикс: из-за
    // перепутанных level/rank в тике падала «Великолепная»).
    assert!(
        tier.iter()
            .all(|d| d.ends_with("_q1") || d == "meat" || d == "offal"),
        "только качество ранга1: {tier:?}"
    );
    assert_eq!(
        info["payload"]["inner"].as_array().expect("inner").len(),
        1,
        "inner не тронут"
    );

    // Повторное снятие шкуры запрещено.
    let ack = harvest(&mut c, &corpse, "skin").await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "повтор skin: {ack}");

    // Снять с другого трупа без ножа нельзя — нож уже есть; проверим
    // запрет разделки без флагов: статический труп Кима (cor_9).
    let ack = harvest(&mut c, "cor_9", "skin").await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "нет флагов: {ack}");
}

/// M22: взаимодействие с предметом во время разделки прерывает её —
/// кожа испорчена («Шкура испорчена», повтор невозможен), освежевание
/// прерывается, но можно повторить.
#[tokio::test]
async fn item_interaction_interrupts_harvest() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    give_gold(&mut c, 1000).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn skinning");
    c.wait_event("evt/craft/profession").await;
    give(&mut c, "skinning_knife").await;
    let potion = give(&mut c, "potion_heal").await;
    let potion2 = give(&mut c, "potion_heal").await;

    let corpse = kill_wolf(&mut c).await;

    // Старт снятия шкуры → взаимодействие с предметом прерывает.
    let ack = harvest(&mut c, &corpse, "skin").await;
    assert_eq!(ack["result"], "ok", "старт skin: {ack}");
    let seq = c.send("cmd/item.use", json!({ "item_uid": potion })).await;
    // Событие прерывания идёт ПЕРЕД ack'ом команды — ждём его первым
    // (иначе wait(ack) поглотит событие).
    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], false, "прервано: {result}");
    assert_eq!(c.ack(seq).await["result"], "ok", "использование зелья");
    assert_eq!(
        result["payload"]["ruined"], true,
        "кожа испорчена: {result}"
    );

    // Повторно снять шкуру нельзя.
    let ack = harvest(&mut c, &corpse, "skin").await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "кожа испорчена: {ack}");

    // Освежевание: прерывание → отмена, повтор разрешён.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["result"], "ok", "старт gut: {ack}");
    // Кулдаун расходников (1 с) — даём зелью отдохнуть.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let seq = c.send("cmd/item.use", json!({ "item_uid": potion2 })).await;
    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], false, "gut прерван: {result}");
    assert_eq!(c.ack(seq).await["result"], "ok", "второе зелье");
    assert_eq!(
        result["payload"]["ruined"], false,
        "gut без порчи: {result}"
    );
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["result"], "ok", "повтор gut разрешён: {ack}");
    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], true, "повтор завершён: {result}");
    let info = {
        let seq = c
            .send("cmd/entity.info", json!({ "entity_id": corpse }))
            .await;
        c.ack(seq).await
    };
    assert_eq!(
        info["payload"]["inner"].as_array().expect("inner").len(),
        1,
        "лут освежевания после повтора: {info}"
    );
}

/// M24: пока идёт разделка, действия персонажа блокируются (E_BUSY);
/// после завершения — снова разрешены. Прерывание уроном и
/// item-командами покрыто отдельно (тик / M23).
#[tokio::test]
async fn harvest_blocks_character_actions() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let corpse = kill_wolf(&mut c).await;

    // Обучение + нож (гейты освежевания — как в прочих тестах).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в деревню");
    give_gold(&mut c, 1000).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn skinning");
    c.wait_event("evt/craft/profession").await;
    give(&mut c, "skinning_knife").await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "назад в лес");

    // Старт освежевания → действия блокируются.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["result"], "ok", "старт gut: {ack}");

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_BUSY",
        "движение заблокировано"
    );
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_BUSY",
        "бой заблокирован"
    );

    // Завершение → действия снова разрешены.
    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], true, "завершён: {result}");
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "после разделки можно");
}

/// Сделать аккаунт админом: роль читается при выдаче сессии — значит
/// до **повторного** входа (паттерн tests/admin.rs).
async fn make_admin(db_url: &str, login: &str) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::connect(&cfg.database).await.expect("БД");
    let n = sqlx::query("UPDATE accounts SET role = 'admin' WHERE login = $1")
        .bind(login)
        .execute(&pool)
        .await
        .expect("UPDATE role")
        .rows_affected();
    assert_eq!(n, 1, "аккаунт найден");
    pool.close().await;
}

/// Мир + админ: повторный вход перечитывает роль из БД.
async fn in_world_admin(ws: &str, db_url: &str) -> (WsClient, String, String) {
    let (mut c, login, char_id) = in_world(ws).await;
    make_admin(db_url, &login).await;
    let seq = c.send("cmd/auth.logout", json!({})).await;
    assert_eq!(c.ack(seq).await["result"], "ok", "logout");
    let seq = c
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "relogin");
    c.wait_event("evt/session.start").await;
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "select");
    c.wait_event("evt/state/snapshot").await;
    (c, login, char_id)
}

/// Убить крысу на складе → id трупа.
async fn kill_rat(c: &mut ld2_test_kit::WsClient, mob: &str) -> String {
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": mob }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака {mob}");
    let corpse = c.wait_event("evt/corpse/created").await;
    let id = corpse["payload"]["entity"]["id"]
        .as_str()
        .expect("id трупа")
        .to_string();
    c.wait_event("evt/combat/end").await;
    id
}

/// M24: старт разделки на ДРУГОМ трупе разрешён и прерывает текущую
/// действие — шкура брошенного трупа портится (повтор снятия невозможен,
/// gut повторить можно); повтор на том же трупе — E_COOLDOWN.
/// Склад: рядом агрессивная огромная крыса — идём в «режиме бога»,
/// чтобы урон не прервал разделку раньше времени.
#[tokio::test]
async fn harvest_switch_to_another_corpse_interrupts_and_ruins() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_admin(&ws, &db_url).await;

    // Режим бога: агрессивная крыса склада не должна уроном
    // прервать разделку (урон прерывает — M24).
    let seq = c
        .send("cmd/admin.god_mode", json!({ "enabled": true }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "god mode");

    // Профессия и нож — в деревне (спавн здесь же).
    give_gold(&mut c, 1000).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn skinning");
    c.wait_event("evt/craft/profession").await;
    give(&mut c, "skinning_knife").await;

    // Склад: деревня → город → юг → склад (без кулдауна движения).
    for exit in ["e_city", "e_south", "e_east"] {
        let seq = c
            .send("cmd/location.move", json!({ "exit_id": exit }))
            .await;
        assert_eq!(c.ack(seq).await["result"], "ok", "переход {exit}");
    }

    // Два трупа неагрессивных крыс (6 hp — умирают за 1–2 раунда).
    let corpse_a = kill_rat(&mut c, "m_rat_1").await;
    let corpse_b = kill_rat(&mut c, "m_rat_2").await;

    // Освежевание A стартовало.
    let ack = harvest(&mut c, &corpse_a, "gut").await;
    assert_eq!(ack["result"], "ok", "старт gut A: {ack}");

    // Переключение на снятие шкуры с B: сервер шлёт итог прерывания
    // и обновление трупа A ДО ack — собираем все три сообщения.
    let seq = c
        .send(
            "cmd/corpse.harvest",
            json!({ "entity_id": corpse_b, "action": "skin" }),
        )
        .await;
    let (ack, interrupted, updated) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut ack = None;
            let mut interrupted = None;
            let mut updated = None;
            while ack.is_none() || interrupted.is_none() || updated.is_none() {
                let v = c.recv_json().await;
                match v["type"].as_str() {
                    Some("ack") if v["seq"] == seq => ack = Some(v),
                    Some("evt/harvest/result") => interrupted = Some(v),
                    Some("evt/corpse/created") if v["payload"]["entity"]["id"] == corpse_a => {
                        updated = Some(v);
                    }
                    _ => {}
                }
            }
            (ack.unwrap(), interrupted.unwrap(), updated.unwrap())
        })
        .await
        .expect("ответ на переключение");
    assert_eq!(ack["result"], "ok", "переключение: {ack}");
    assert_eq!(
        interrupted["payload"]["entity_id"], corpse_a,
        "итог прерывания — по A"
    );
    assert_eq!(interrupted["payload"]["ok"], false, "A прервано");
    assert_eq!(interrupted["payload"]["ruined"], true, "шкура A испорчена");
    assert_eq!(
        updated["payload"]["entity"]["harvest"]["skin_done"], true,
        "флаги трупа A обновлены в локации"
    );

    // Снятие шкуры с B завершается как обычно.
    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], true, "skin B готов: {result}");
    assert_eq!(result["payload"]["entity_id"], corpse_b, "итог по B");

    // A: снять шкуру нельзя (испорчена), gut повторить можно.
    let ack = harvest(&mut c, &corpse_a, "skin").await;
    assert_eq!(
        ack["error"]["code"], "E_ITEM_GONE",
        "шкура A испорчена: {ack}"
    );
    let ack = harvest(&mut c, &corpse_a, "gut").await;
    assert_eq!(ack["result"], "ok", "gut A повторно: {ack}");
}

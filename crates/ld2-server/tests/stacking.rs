//! M18/F58f: рюкзак вмещает **стаки**, а не предметы (БД-тест):
//! однотипное складывается в существующий простой стак, лимит слотов
//! считает записи, «грязные» экземпляры не сливаются (гейт:
//! `LD2_TEST_DATABASE_URL`).

use ld2_server::config::DatabaseSection;
use ld2_server::db;

fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

#[tokio::test]
async fn backpack_counts_stacks_not_items() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let cfg = DatabaseSection {
        url,
        max_connections: 4,
    };
    let pool = db::connect(&cfg).await.expect("connect");

    // Уникальные сущности (повторные прогоны не конфликтуют).
    let uniq = format!("st_{}", std::process::id());
    let login = format!("t_{uniq}");
    let account_id: i64 = sqlx::query_scalar(
        "INSERT INTO accounts (login, password_hash) VALUES ($1, 'x') RETURNING id",
    )
    .bind(&login)
    .fetch_one(&pool)
    .await
    .expect("account");
    let char_name = format!("Тест {uniq}");
    let char_id: String = sqlx::query_scalar(
        "INSERT INTO characters (account_id, name, profile) VALUES ($1, $2, 'warrior') RETURNING id",
    )
    .bind(account_id)
    .bind(&char_name)
    .fetch_one(&pool)
    .await
    .expect("character");

    let def_a = format!("t_stack_a_{uniq}");
    let def_b = format!("t_stack_b_{uniq}");
    for def_id in [&def_a, &def_b] {
        sqlx::query(
            "INSERT INTO item_defs (id, item_type, name, rarity, gold_value) \
             VALUES ($1, 'material', 'Тестовый', 'common', 1)",
        )
        .bind(def_id)
        .execute(&pool)
        .await
        .expect("item_def");
    }

    // 1) Два приёма однотипного → один стак (один слот), qty складывается.
    let (first, stacked) = db::items::grant_stacking_limited(&pool, &char_id, &def_a, 3, 40)
        .await
        .expect("grant 1")
        .expect("место есть");
    assert!(!stacked, "первый приём — новая запись");
    let (second, stacked) = db::items::grant_stacking_limited(&pool, &char_id, &def_a, 2, 40)
        .await
        .expect("grant 2")
        .expect("стак существует — лимит не нужен");
    assert!(stacked, "второй приём — в стак");
    assert_eq!(second.uid, first.uid, "тот же стак");
    assert_eq!(second.qty, 5, "3 + 2");
    assert_eq!(
        db::items::count(&pool, &char_id).await.expect("count"),
        1,
        "два приёма = один занятый слот"
    );

    // 2) Лимит считается по стакам (записям): slots=1 → второй стак не влезает.
    let res = db::items::grant_stacking_limited(&pool, &char_id, &def_b, 1, 1)
        .await
        .expect("limit query");
    assert!(res.is_none(), "слот один — новый стак не помещается");
    assert_eq!(
        db::items::count(&pool, &char_id).await.expect("count"),
        1,
        "отказ ничего не добавил"
    );

    // 3) Стак не занимает место: при полном рюкзаке прибавка в стак идёт.
    let (row, stacked) = db::items::grant_stacking_limited(&pool, &char_id, &def_a, 7, 1)
        .await
        .expect("grant")
        .expect("стак есть — место не нужно");
    assert!(stacked, "прибавка в существующий стак");
    assert_eq!(row.qty, 12, "5 + 7");

    // 4) Подбор лута (take_loot_tx) тоже складывается.
    let (row, stacked) = db::items::take_loot_tx(&pool, "loot:t:1", &char_id, &def_a, 1)
        .await
        .expect("loot 1")
        .expect("не собран");
    assert!(stacked, "лут идёт в стак");
    assert_eq!(row.qty, 13, "12 + 1");
    // Повторный uid лута — уже собран.
    let dup = db::items::take_loot_tx(&pool, "loot:t:1", &char_id, &def_a, 1)
        .await
        .expect("dup");
    assert!(dup.is_none(), "однократность лута сохраняется");

    // 5) «Грязный» экземпляр (улучшенный) не складывается с простым.
    let (dirty, stacked) = db::items::grant_stacking_limited(&pool, &char_id, &def_b, 1, 40)
        .await
        .expect("grant b")
        .expect("место есть");
    assert!(!stacked, "новый def — новая запись");
    sqlx::query("UPDATE items SET upgrade = 1 WHERE uid = $1")
        .bind(&dirty.uid)
        .execute(&pool)
        .await
        .expect("upgrade");
    let (next, stacked) = db::items::grant_stacking_limited(&pool, &char_id, &def_b, 1, 40)
        .await
        .expect("grant b2")
        .expect("место есть");
    assert!(
        !stacked,
        "улучшенный не складывается — состояние сохранилось"
    );
    assert_ne!(next.uid, dirty.uid, "отдельная запись");
    assert_eq!(dirty.qty, 1, "грязный стак не вырос");

    // Уборка.
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

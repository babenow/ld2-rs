//! M28/B2: контейнеры инвентаря — рюкзак и надетые сумки (гейт:
//! `LD2_TEST_DATABASE_URL`).

use ld2_server::config::DatabaseSection;
use ld2_server::db;
use ld2_server::db::items::{PlaceError, BACKPACK, BAGS};

fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

async fn setup(pool: &sqlx::PgPool, uniq: &str, bag_slots: i32) -> (String, String, String) {
    let account_id: i64 = sqlx::query_scalar(
        "INSERT INTO accounts (login, password_hash) VALUES ($1, 'x') RETURNING id",
    )
    .bind(format!("bags_{uniq}"))
    .fetch_one(pool)
    .await
    .expect("account");
    let char_id: String = sqlx::query_scalar(
        "INSERT INTO characters (account_id, name, profile) \
         VALUES ($1, $2, 'warrior') RETURNING id",
    )
    .bind(account_id)
    .bind(format!("Сумки {uniq}"))
    .fetch_one(pool)
    .await
    .expect("character");
    let bag_def = format!("t_bag_{uniq}");
    let item_def = format!("t_item_{uniq}");
    for (id, ty, slots) in [
        (&bag_def, "bag", Some(bag_slots)),
        (&item_def, "material", None),
    ] {
        sqlx::query(
            "INSERT INTO item_defs (id, item_type, name, rarity, gold_value, bag_slots) \
             VALUES ($1, $2, 'Тест', 'common', 1, $3)",
        )
        .bind(id)
        .bind(ty)
        .bind(slots)
        .execute(pool)
        .await
        .expect("item_def");
    }
    (char_id, bag_def, item_def)
}

#[tokio::test]
async fn bags_add_capacity_and_hold_items() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let pool = db::connect(&DatabaseSection {
        url,
        max_connections: 4,
    })
    .await
    .expect("connect");
    let uniq = format!("c_{}", std::process::id());
    let (char_id, bag_def, item_def) = setup(&pool, &uniq, 20).await;

    // База без сумок: 20 ячеек, занято 0.
    assert_eq!(db::items::capacity(&pool, &char_id, 20).await.unwrap(), 20);
    assert_eq!(db::items::used(&pool, &char_id).await.unwrap(), 0);

    // Стартовые сумки: 4 × 20 → ёмкость 100, сами сумки место не занимают.
    let granted = db::items::grant_bags(&pool, &char_id, 4, &bag_def)
        .await
        .expect("grant_bags");
    assert_eq!(granted, 4);
    assert_eq!(db::items::capacity(&pool, &char_id, 20).await.unwrap(), 100);
    assert_eq!(db::items::used(&pool, &char_id).await.unwrap(), 0);

    // Предмет в рюкзак, затем внутрь первой сумки.
    let item = db::items::insert(&pool, &char_id, &item_def, 1)
        .await
        .expect("insert");
    db::items::place(&pool, &char_id, &item.uid, BACKPACK, 0, 20, 4)
        .await
        .expect("в рюкзак");
    let bag_uid: String =
        sqlx::query_scalar("SELECT uid FROM items WHERE character_id = $1 AND container = 'bags' ORDER BY slot_index LIMIT 1")
            .bind(&char_id)
            .fetch_one(&pool)
            .await
            .expect("bag uid");
    db::items::place(&pool, &char_id, &item.uid, &bag_uid, 0, 20, 4)
        .await
        .expect("в сумку");
    assert!(db::items::bag_has_contents(&pool, &bag_uid).await.unwrap());

    // Непустую сумку снять нельзя; опустошили — можно.
    assert_eq!(
        db::items::place(&pool, &char_id, &bag_uid, BACKPACK, 1, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::BagNotEmpty
    );
    db::items::place(&pool, &char_id, &item.uid, BACKPACK, 1, 20, 4)
        .await
        .expect("обратно в рюкзак");
    db::items::place(&pool, &char_id, &bag_uid, BACKPACK, 2, 20, 4)
        .await
        .expect("сумка снята");

    // Сумку в сумку класть нельзя.
    let equipped: String =
        sqlx::query_scalar("SELECT uid FROM items WHERE character_id = $1 AND container = 'bags' ORDER BY slot_index LIMIT 1")
            .bind(&char_id)
            .fetch_one(&pool)
            .await
            .expect("equipped bag");
    assert_eq!(
        db::items::place(&pool, &char_id, &bag_uid, &equipped, 0, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::BagInBag
    );
}

#[tokio::test]
async fn place_swaps_and_bounds() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let pool = db::connect(&DatabaseSection {
        url,
        max_connections: 4,
    })
    .await
    .expect("connect");
    let uniq = format!("s_{}", std::process::id());
    let (char_id, _bag_def, item_def) = setup(&pool, &uniq, 20).await;

    let a = db::items::insert(&pool, &char_id, &item_def, 1)
        .await
        .unwrap();
    let b = db::items::insert(&pool, &char_id, &item_def, 2)
        .await
        .unwrap();
    db::items::place(&pool, &char_id, &a.uid, BACKPACK, 0, 20, 4)
        .await
        .unwrap();
    db::items::place(&pool, &char_id, &b.uid, BACKPACK, 1, 20, 4)
        .await
        .unwrap();
    // Обмен местами: b → 0, a → 1.
    db::items::place(&pool, &char_id, &b.uid, BACKPACK, 0, 20, 4)
        .await
        .unwrap();
    let slot = |uid: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<i32>>("SELECT slot_index FROM items WHERE uid = $1")
                .bind(uid)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(slot(b.uid.clone()).await, Some(0));
    assert_eq!(slot(a.uid.clone()).await, Some(1));
    // За пределами базы — OutOfRange.
    assert_eq!(
        db::items::place(&pool, &char_id, &a.uid, BACKPACK, 20, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::OutOfRange
    );
    // Занятый слот сумки при надевании — BagBusy.
    let bag_def: String = sqlx::query_scalar(
        "SELECT id FROM item_defs WHERE item_type = 'bag' AND id LIKE $1 LIMIT 1",
    )
    .bind(format!("t_bag_{uniq}"))
    .fetch_one(&pool)
    .await
    .unwrap();
    db::items::grant_bags(&pool, &char_id, 1, &bag_def)
        .await
        .unwrap();
    let second_bag = db::items::insert(&pool, &char_id, &bag_def, 1)
        .await
        .unwrap();
    assert_eq!(
        db::items::place(&pool, &char_id, &second_bag.uid, BAGS, 0, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::BagBusy
    );
}

/// M28: сумка с `bag_types` принимает только перечисленные типы.
#[tokio::test]
async fn typed_bag_rejects_other_types() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let pool = db::connect(&DatabaseSection {
        url,
        max_connections: 4,
    })
    .await
    .expect("connect");
    let uniq = format!("ty_{}", std::process::id());
    let (char_id, bag_def, _mat) = setup(&pool, &uniq, 10).await;
    sqlx::query("UPDATE item_defs SET bag_types = ARRAY['herb']::text[] WHERE id = $1")
        .bind(&bag_def)
        .execute(&pool)
        .await
        .expect("bag_types");

    let herb_def = format!("t_herb_{uniq}");
    let sword_def = format!("t_sword_{uniq}");
    for (id, ty) in [(&herb_def, "herb"), (&sword_def, "weapon")] {
        sqlx::query(
            "INSERT INTO item_defs (id, item_type, name, rarity, gold_value) \
             VALUES ($1, $2, 'Тест', 'common', 1)",
        )
        .bind(id)
        .bind(ty)
        .execute(&pool)
        .await
        .expect("item_def");
    }

    db::items::grant_bags(&pool, &char_id, 1, &bag_def)
        .await
        .expect("grant_bags");
    let bag_uid: String = sqlx::query_scalar(
        "SELECT uid FROM items WHERE character_id = $1 AND container = 'bags' LIMIT 1",
    )
    .bind(&char_id)
    .fetch_one(&pool)
    .await
    .expect("bag uid");
    let herb = db::items::insert(&pool, &char_id, &herb_def, 1)
        .await
        .expect("herb");
    let sword = db::items::insert(&pool, &char_id, &sword_def, 1)
        .await
        .expect("sword");

    // Траву — можно, оружие — нет (E_BAG_TYPE → WrongType).
    db::items::place(&pool, &char_id, &herb.uid, &bag_uid, 0, 20, 4)
        .await
        .expect("herb в сумку травника");
    assert_eq!(
        db::items::place(&pool, &char_id, &sword.uid, &bag_uid, 1, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::WrongType
    );
    // В обычный рюкзак оружие кладётся.
    db::items::place(&pool, &char_id, &sword.uid, BACKPACK, 3, 20, 4)
        .await
        .expect("sword в рюкзак");
}

/// M28/B8: спец-слоты — реагенты (bag_slots+0), колчан (+1),
/// ключница (+2); обычная сумка туда не встаёт, спец-сумка — только
/// в свой слот.
#[tokio::test]
async fn special_bag_slots_require_matching_kind() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let pool = db::connect(&DatabaseSection {
        url,
        max_connections: 4,
    })
    .await
    .expect("connect");
    let uniq = format!("sp_{}", std::process::id());
    let (char_id, general_def, _mat) = setup(&pool, &uniq, 4).await;
    let reagent_def = format!("t_reagent_{uniq}");
    let quiver_def = format!("t_quiver_{uniq}");
    let keyring_def = format!("t_keyring_{uniq}");
    for (id, slots, kind) in [
        (&reagent_def, 5, "reagent"),
        (&quiver_def, 2, "quiver"),
        (&keyring_def, 3, "keyring"),
    ] {
        sqlx::query(
            "INSERT INTO item_defs (id, item_type, name, rarity, gold_value, bag_slots, bag_kind) \
             VALUES ($1, 'bag', 'Тест', 'common', 1, $2, $3)",
        )
        .bind(id)
        .bind(slots)
        .bind(kind)
        .execute(&pool)
        .await
        .expect("item_def");
    }

    let general = db::items::insert(&pool, &char_id, &general_def, 1)
        .await
        .expect("general");
    let reagent = db::items::insert(&pool, &char_id, &reagent_def, 1)
        .await
        .expect("reagent");
    let quiver = db::items::insert(&pool, &char_id, &quiver_def, 1)
        .await
        .expect("quiver");
    let keyring = db::items::insert(&pool, &char_id, &keyring_def, 1)
        .await
        .expect("keyring");

    // Общие слоты 0..3: подходит только обычная сумка.
    db::items::place(&pool, &char_id, &general.uid, BAGS, 0, 20, 4)
        .await
        .expect("обычная сумка в общий слот");
    assert_eq!(
        db::items::place(&pool, &char_id, &reagent.uid, BAGS, 1, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::WrongSlot
    );
    // Спец-слоты: 4 — реагенты, 5 — колчан, 6 — ключница.
    assert_eq!(
        db::items::place(&pool, &char_id, &general.uid, BAGS, 4, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::WrongSlot
    );
    db::items::place(&pool, &char_id, &reagent.uid, BAGS, 4, 20, 4)
        .await
        .expect("сумка реагентов в свой слот");
    // Занятый спец-слот сообщает BagBusy (свап сумок запрещён).
    assert_eq!(
        db::items::place(&pool, &char_id, &quiver.uid, BAGS, 4, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::BagBusy
    );
    db::items::place(&pool, &char_id, &quiver.uid, BAGS, 5, 20, 4)
        .await
        .expect("колчан в свой слот");
    assert_eq!(
        db::items::place(&pool, &char_id, &keyring.uid, BAGS, 5, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::BagBusy
    );
    db::items::place(&pool, &char_id, &keyring.uid, BAGS, 6, 20, 4)
        .await
        .expect("ключница в свой слот");
    // Внешний индекс за спец-слотами — вне ёмкости.
    assert_eq!(
        db::items::place(&pool, &char_id, &reagent.uid, BAGS, 7, 20, 4)
            .await
            .unwrap_err(),
        PlaceError::OutOfRange
    );
}

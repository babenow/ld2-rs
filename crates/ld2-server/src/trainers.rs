//! Тренеры (M14, F5): обучение и вложение XP — только у NPC-тренера в
//! локации (`E_NO_TRAINER`), оплата — по цене из маркера контента
//! (`locations.json → entities[].trainer`).
//!
//! Маркер: `{"trainer": {"professions": [...], "skills": [...],
//! "price": {"gold_base": N, "gold_per_rank": M, "items": [{type, qty}]}}}`.
//! Итоговая цена золота растёт с рангом владения: `base + per_rank × ранг`.

use sqlx::PgPool;

use crate::db;

/// Цена обучения у тренера (из маркера контента).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrainerPrice {
    pub gold_base: i64,
    pub gold_per_rank: i64,
    /// Предметы-квестоусловия («принеси мне что-то»): (item_type, qty).
    pub items: Vec<(String, i64)>,
}

/// Что ищем у тренера: профессия (`craft.learn`) или навык (`xp_invest`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainerKind {
    Profession,
    Skill,
}

/// Золото к уплате: `gold_base + gold_per_rank × ранг` (M14: цена растёт
/// с уровнем владения). Чистая функция (юнит-тест).
pub fn gold_price(price: &TrainerPrice, rank: i64) -> i64 {
    (price.gold_base + price.gold_per_rank * rank.max(0)).max(0)
}

/// Тренер в локации, обучающий `target` (id профессии/навыка) → его цена;
/// `None` — такого тренера нет (`E_NO_TRAINER`).
pub async fn trainer_price(
    pool: &PgPool,
    location_id: &str,
    kind: TrainerKind,
    target: &str,
) -> Result<Option<TrainerPrice>, sqlx::Error> {
    let Some(loc) = db::locations::get(pool, location_id).await? else {
        return Ok(None);
    };
    let entities: Vec<ld2_protocol::types::Entity> =
        serde_json::from_value(loc.entities).unwrap_or_default();
    for entity in &entities {
        let Some(marker) = entity.extra.get("trainer") else {
            continue;
        };
        let list_key = match kind {
            TrainerKind::Profession => "professions",
            TrainerKind::Skill => "skills",
        };
        let teaches = marker
            .get(list_key)
            .and_then(|v| v.as_array())
            .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(target)));
        if !teaches {
            continue;
        }
        let price = marker.get("price").cloned().unwrap_or_default();
        let items = price
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|it| {
                        Some((
                            it.get("type")?.as_str()?.to_string(),
                            it.get("qty").and_then(|q| q.as_i64()).unwrap_or(1),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(Some(TrainerPrice {
            gold_base: price.get("gold_base").and_then(|v| v.as_i64()).unwrap_or(0),
            gold_per_rank: price
                .get("gold_per_rank")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            items,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gold_price_grows_with_rank() {
        let price = TrainerPrice {
            gold_base: 100,
            gold_per_rank: 50,
            items: vec![],
        };
        assert_eq!(gold_price(&price, 0), 100, "ранг 0 — базовая цена");
        assert_eq!(gold_price(&price, 3), 250, "100 + 50×3");
        // Отрицательный ранг не даёт минус.
        assert_eq!(gold_price(&price, -5), 100);
    }

    #[test]
    fn marker_price_items_parsing() {
        let marker: serde_json::Value = serde_json::json!({
            "professions": ["alchemist"],
            "skills": ["sword"],
            "price": {"gold_base": 100, "gold_per_rank": 50,
                      "items": [{"type": "quest_token", "qty": 2}]}
        });
        let items: Vec<(String, i64)> = marker["price"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                (
                    it["type"].as_str().unwrap().to_string(),
                    it["qty"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(items, vec![("quest_token".to_string(), 2)]);
        assert_eq!(marker["price"]["gold_base"].as_i64(), Some(100));
    }
}

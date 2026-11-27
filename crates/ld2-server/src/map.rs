//! M26: карта-граф мира (`cmd/map.world`): статический граф локаций
//! контента в проводном виде + чанкизация по байтовому бюджету WS
//! (`MAX_MESSAGE_BYTES` = 64 КБ — полный граф ~0.5 МБ в одно
//! сообщение не влезает; порядок стабильный — по id).

use std::collections::HashSet;

use sqlx::PgPool;

/// Кэш карты мира: локации в проводном виде + размеры (чанкизация
/// по байтовому бюджету без повторных сериализаций).
pub struct MapCache {
    locations: Vec<serde_json::Value>,
    sizes: Vec<usize>,
    /// Корень раскладки карты (спавн-локация) — клиент ставит
    /// компасный BFS от него (стабильная карта мира).
    root: String,
}

impl MapCache {
    /// Построить из БД (один раз на процесс — граф статичен,
    /// контент-сид). Висячие выходы (`to` вне контента)
    /// отфильтровываются — на карте их быть не должно.
    pub async fn build(pool: &PgPool) -> Result<Self, sqlx::Error> {
        let rows = crate::db::locations::map_rows(pool).await?;
        let ids: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        let mut dropped = 0usize;
        let mut locations = Vec::with_capacity(rows.len());
        let mut sizes = Vec::with_capacity(rows.len());
        for row in &rows {
            let exits: Vec<serde_json::Value> = row
                .exits
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter(|exit| {
                            let to = exit.get("to").and_then(|v| v.as_str()).unwrap_or("");
                            if ids.contains(to) {
                                true
                            } else {
                                dropped += 1;
                                false
                            }
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let value = serde_json::json!({
                "id": row.id,
                "name": row.name,
                "text": row.text,
                "exits": exits,
            });
            sizes.push(serde_json::to_vec(&value).map(|v| v.len()).unwrap_or(0));
            locations.push(value);
        }
        if dropped > 0 {
            tracing::warn!("карта мира: отфильтровано висячих выходов: {dropped}");
        }
        // Корень раскладки — спавн-точка (`meta.spawn_location`);
        // fallback — первая по id (стабильный порядок).
        let spawn = crate::db::locations::spawn_id(pool).await?;
        let root = spawn
            .filter(|s| ids.contains(s.as_str()))
            .or_else(|| rows.first().map(|r| r.id.clone()))
            .unwrap_or_default();
        if root.is_empty() {
            tracing::warn!("карта мира: нет спавн-корня (пустой контент?)");
        }
        Ok(Self {
            locations,
            sizes,
            root,
        })
    }

    /// Корень раскладки (id спавн-локации).
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Всего локаций в графе.
    pub fn total(&self) -> usize {
        self.locations.len()
    }

    /// Чанк начиная с `offset` в пределах `budget` байт (минимум —
    /// одна локация, чтобы чанк всегда продвигался). Возвращает срез
    /// и следующий offset (`>= total` — конец).
    pub fn chunk(&self, offset: usize, budget: usize) -> (&[serde_json::Value], usize) {
        let start = offset.min(self.locations.len());
        let mut end = start;
        let mut used = 0usize;
        while end < self.locations.len() {
            // +1 — запятая при склейке массива.
            let next = self.sizes[end] + 1;
            if end > start && used + next > budget {
                break;
            }
            used += next;
            end += 1;
        }
        (&self.locations[start..end], end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(values: &[&str]) -> MapCache {
        let locations: Vec<serde_json::Value> = values
            .iter()
            .map(|v| serde_json::json!({ "id": v }))
            .collect();
        let sizes = locations
            .iter()
            .map(|v| serde_json::to_vec(v).map(|b| b.len()).unwrap_or(0))
            .collect();
        MapCache {
            locations,
            sizes,
            root: "root".into(),
        }
    }

    /// M26: чанк всегда продвигается (минимум — одна локация) и не
    /// превышает бюджет (кроме гигантской первой локации).
    #[test]
    fn chunk_respects_budget_and_progresses() {
        let c = cache(&["a", "b", "c", "d"]);
        let (first, next) = c.chunk(0, 10);
        assert_eq!(first.len(), 1, "маленький бюджет → по одной");
        assert_eq!(next, 1);
        let (rest, end) = c.chunk(1, 10_000);
        assert_eq!(rest.len(), 3, "большой бюджет → всё остальное");
        assert_eq!(end, 4);
        let (empty, end) = c.chunk(4, 10_000);
        assert!(empty.is_empty(), "за концом — пусто");
        assert_eq!(end, 4, "offset не уходит за total");
        let (overflow, end) = c.chunk(99, 10_000);
        assert!(overflow.is_empty(), "offset за пределами → пусто");
        assert_eq!(end, 4);
    }
}

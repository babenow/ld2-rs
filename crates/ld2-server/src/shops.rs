//! M18/F39: **торговый магазин NPC** — контент `content/shops.json`
//! (наценка торговца + валютные вкладки с товарами), загрузка в память
//! при старте и чистые функции цены: база `gold_value` предмета ×
//! наценка сервера (конфиг) × наценка торговца (контент) × скидка
//! (аккаунт + навыки, bp с потолком), минимум 1; конвертация в валюту
//! вкладки по `rate`.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::content::ContentError;

/// Корень `content/shops.json` (по образцу прочих файлов каталога).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShopsRoot {
    #[serde(default)]
    pub npcs: Vec<ShopNpc>,
}

/// Магазин одного NPC.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ShopNpc {
    pub id: String,
    /// Наценка торговца (1.15 = +15% к базовой цене предмета).
    pub markup: f64,
    /// Валютные вкладки: чем и за что торгует.
    #[serde(default)]
    pub currencies: Vec<ShopCurrency>,
}

/// Валютная вкладка магазина.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ShopCurrency {
    /// `def_id` валюты (напр. `gold_coin`, `gem_str`).
    pub currency: String,
    /// Сколько единиц валюты за золотой эквивалент (gold_coin → 1.0;
    /// самоцветы → дробь, если золото дороже).
    #[serde(default = "default_rate")]
    pub rate: f64,
    /// `def_id` товаров этой вкладки.
    #[serde(default)]
    pub items: Vec<String>,
}

fn default_rate() -> f64 {
    1.0
}

/// M18/F39: цена покупки в золотом эквиваленте: база × наценка сервера
/// × наценка торговца × (1 − скидка bp с потолком); минимум 1.
/// Эпсилон перед floor гасит ошибку представления (180 × 1.15 =
/// 206.999… → 207). Чистая функция (юнит-тесты).
pub fn buy_price_gold(
    base: i64,
    server_markup: f64,
    npc_markup: f64,
    discount_bp: i64,
    max_discount_bp: i64,
) -> i64 {
    let discount = discount_bp.clamp(0, max_discount_bp).max(0) as f64 / 10_000.0;
    let price = base as f64 * server_markup * npc_markup * (1.0 - discount);
    (price + 1e-9).floor().max(1.0) as i64
}

/// M18/F39: конвертация золотого эквивалента в валюту вкладки
/// (`rate` — единиц валюты за золото); минимум 1.
pub fn price_in_currency(price_gold: i64, rate: f64) -> i64 {
    ((price_gold as f64) * rate).round().max(1.0) as i64
}

/// Скидка навыков в bp: Σ(rank × множитель_за_ранг × 10 000) —
/// множители из `[gameplay.shop].skill_discounts` (напр. haggle 0.02
/// = 2% за ранг). Чистая функция (юнит-тесты).
pub fn skill_discount_bp(
    skill_discounts: &HashMap<String, f64>,
    ranks: &HashMap<String, i64>,
) -> i64 {
    skill_discounts
        .iter()
        .map(|(skill, per_rank)| {
            let rank = ranks.get(skill).copied().unwrap_or(0).max(0);
            (*per_rank * 10_000.0 * rank as f64) as i64
        })
        .sum()
}

/// Загрузка `shops.json`: файла нет → пусто (магазины опциональны),
/// сломанный JSON → ошибка (в `AppState::new` — warn + пусто;
/// валидность ловят контент-тесты в CI).
pub fn load(dir: &Path) -> Result<HashMap<String, ShopNpc>, ContentError> {
    let path = dir.join("shops.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| ContentError::Io(path.clone(), e))?;
    let root: ShopsRoot = serde_json::from_str(&raw).map_err(|e| ContentError::Json(path, e))?;
    Ok(root
        .npcs
        .into_iter()
        .map(|shop| (shop.id.clone(), shop))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M18/F39: формула цены — наценки умножаются, скидка bp с
    /// потолком, минимум 1.
    #[test]
    fn price_applies_markups_and_capped_discount() {
        // База 100: наценки 1.1 (сервер) × 1.15 (торговец) → 126.5 → 126.
        assert_eq!(buy_price_gold(100, 1.1, 1.15, 0, 5000), 126);
        // Скидка 10% (1000 bp) → 126 × 0.9 = 113.4 → 113.
        assert_eq!(buy_price_gold(100, 1.1, 1.15, 1000, 5000), 113);
        // Потолок скидки: 80% требуют, потолок 30% (3000 bp).
        assert_eq!(buy_price_gold(100, 1.0, 1.0, 8000, 3000), 70);
        // Отрицательная/нулевая скидка не поднимает цену.
        assert_eq!(buy_price_gold(100, 1.0, 1.0, -500, 5000), 100);
        // Крошечная база × скидка 100% потолком 100% → минимум 1.
        assert_eq!(buy_price_gold(1, 1.0, 1.0, 10_000, 10_000), 1);
        // Без наценок — ровно база.
        assert_eq!(buy_price_gold(42, 1.0, 1.0, 0, 5000), 42);
        // Регрессия IEEE: 180 × 1.15 = 206.999… → 207 (эпсилон).
        assert_eq!(buy_price_gold(180, 1.0, 1.15, 0, 5000), 207);
    }

    /// M18/F39: конвертация валюты — rate, округление, минимум 1.
    #[test]
    fn currency_rate_converts_and_keeps_minimum() {
        assert_eq!(price_in_currency(100, 1.0), 100);
        // Золото дороже самоцвета: 100г → 2 самоцвета (rate 0.02).
        assert_eq!(price_in_currency(100, 0.02), 2);
        assert_eq!(price_in_currency(10, 0.04), 1);
        // Дробная цена валюты не опускается ниже 1.
        assert_eq!(price_in_currency(1, 0.01), 1);
    }

    /// M18/F39: скидка навыков — bp за ранг по каждому навыку из
    /// конфига, только изученные ранги.
    #[test]
    fn skill_discount_sums_learned_ranks() {
        let mut cfg = HashMap::new();
        cfg.insert("haggle".to_string(), 0.02); // 2% за ранг
        cfg.insert("lore".to_string(), 0.01); // 1% за ранг
        let mut ranks = HashMap::new();
        ranks.insert("haggle".to_string(), 3i64); // 6%
        ranks.insert("lore".to_string(), 0i64); // не изучен
                                                // 3 × 0.02 × 10000 = 600 bp.
        assert_eq!(skill_discount_bp(&cfg, &ranks), 600);
        ranks.insert("lore".to_string(), 2i64); // +200 bp
        assert_eq!(skill_discount_bp(&cfg, &ranks), 800);
        // Незнакомый навык ранга 0 даёт 0.
        let empty = HashMap::new();
        assert_eq!(skill_discount_bp(&empty, &ranks), 0);
    }
}

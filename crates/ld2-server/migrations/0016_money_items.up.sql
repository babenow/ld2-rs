-- F29a (M16): деньги становятся предметами-монетами: колонка
-- characters.gold переносится в стак gold_coin (item_type=money),
-- колонка убирается. Определение вставляется здесь же — миграции
-- применяются ДО сида контента; ON CONFLICT — сид перезапишет
-- (значения зеркалят content/items.json).
INSERT INTO item_defs (id, item_type, name, rarity, upgrade, stats, gold_value, qty)
VALUES ('gold_coin', 'money', 'Золотая монета', 'common', 0, '{}', 1, 10)
ON CONFLICT (id) DO NOTHING;

INSERT INTO items (character_id, def_id, qty)
SELECT id, 'gold_coin', gold FROM characters WHERE gold > 0;

ALTER TABLE characters DROP COLUMN gold;

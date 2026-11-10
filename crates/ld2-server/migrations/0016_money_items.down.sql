-- F29a: обратно — колонка золота из стаков монет (определение
-- не трогаем: владелец — сид контента).
ALTER TABLE characters ADD COLUMN gold BIGINT NOT NULL DEFAULT 0;

UPDATE characters c
SET gold = COALESCE(
    (
        SELECT SUM(i.qty)
        FROM items i
        JOIN item_defs d ON d.id = i.def_id
        WHERE i.character_id = c.id AND d.item_type = 'money'
    ),
    0
);

DELETE FROM items i
USING item_defs d
WHERE d.id = i.def_id AND d.item_type = 'money';

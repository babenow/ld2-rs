-- M16/F29g: порядок предметов в рюкзаке (перетаскивание) —
-- позиция персонажа; новые строки получают MAX+1 при вставке.
ALTER TABLE items ADD COLUMN sort_order INTEGER;

UPDATE items i
SET sort_order = sub.rn
FROM (
    SELECT uid, ROW_NUMBER() OVER (PARTITION BY character_id ORDER BY created_at, uid) AS rn
    FROM items
) sub
WHERE i.uid = sub.uid;

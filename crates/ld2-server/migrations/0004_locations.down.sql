-- Откат миграции 0004 (локации + последний персонаж, M12.3).
ALTER TABLE accounts DROP COLUMN IF EXISTS last_char_id;
DROP TABLE IF EXISTS locations;

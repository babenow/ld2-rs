-- Откат миграции 0006 (сокеты/навык/преступность, M12.5). Данные теряются.
DROP INDEX IF EXISTS crime_log_character_idx;
DROP TABLE IF EXISTS crime_log;
DROP TABLE IF EXISTS crime_statuses;
ALTER TABLE item_defs DROP COLUMN IF EXISTS skill;
ALTER TABLE items DROP COLUMN IF EXISTS sockets;

-- Откат миграции 0005 (навыки/предметы/атрибуты, M12.4). Данные теряются.
DROP TABLE IF EXISTS loot_taken;
DROP TABLE IF EXISTS items;
DROP TABLE IF EXISTS skills;
DROP TABLE IF EXISTS item_defs;
DROP TABLE IF EXISTS character_skills;
ALTER TABLE characters DROP COLUMN IF EXISTS attrs;

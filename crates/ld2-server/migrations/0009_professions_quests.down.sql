-- Откат миграции 0009 (профессии/рецепты/квесты/крафт, M12.6). Данные теряются.
DROP INDEX IF EXISTS craft_jobs_character_idx;
DROP INDEX IF EXISTS craft_jobs_ready_idx;
DROP TABLE IF EXISTS craft_jobs;
DROP TABLE IF EXISTS character_quests;
DROP TABLE IF EXISTS character_professions;
DROP TABLE IF EXISTS quests;
DROP TABLE IF EXISTS recipes;
DROP TABLE IF EXISTS professions;

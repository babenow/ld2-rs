-- Откат миграции 0003 (персонажи, M12.2): индексы и таблица удаляются.
-- Порядок: сначала индексы, затем таблица. Данные персонажей теряются.
DROP INDEX IF EXISTS characters_account_idx;
DROP INDEX IF EXISTS characters_name_alive_idx;
DROP TABLE IF EXISTS characters;

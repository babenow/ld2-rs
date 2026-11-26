-- Откат миграции 0010 (история чата, M12.7). Данные теряются.
DROP INDEX IF EXISTS chat_history_channel_idx;
DROP TABLE IF EXISTS chat_history;

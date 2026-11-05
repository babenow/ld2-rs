-- M12.1 (PLAN-M12): служебные ключи сервера.
-- Игровые таблицы добавляются миграциями по фазам (раздел «Схема БД» плана).
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

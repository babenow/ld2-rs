-- Откат миграции 0008 (золото персонажей, M12.5c). Данные теряются.
ALTER TABLE characters DROP COLUMN IF EXISTS gold;

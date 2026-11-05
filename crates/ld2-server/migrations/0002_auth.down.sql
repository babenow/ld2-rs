-- Откат миграции 0002 (аккаунты и токены, M12.2): сначала токены (FK на
-- accounts), затем аккаунты. Данные аккаунтов теряются.
DROP INDEX IF EXISTS refresh_tokens_account_idx;
DROP TABLE IF EXISTS refresh_tokens;
DROP TABLE IF EXISTS accounts;

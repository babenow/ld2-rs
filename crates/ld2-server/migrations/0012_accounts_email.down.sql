-- Откат миграции 0012 (email аккаунтов, M13.1). Данные email теряются.
DROP INDEX IF EXISTS accounts_email_idx;
ALTER TABLE accounts DROP COLUMN IF EXISTS email;

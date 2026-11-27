-- M33: премиум-аккаунт — срок действия (epoch-секунды).
-- NULL или прошедшее время — премиум неактивен.
ALTER TABLE accounts ADD COLUMN premium_until BIGINT;

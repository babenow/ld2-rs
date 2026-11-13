-- M18/F39: скидка аккаунта в магазинах NPC (basis points, 1000 = −10%).
-- Премиум-аккаунты активируются вручную (биллинга нет):
--   UPDATE accounts SET shop_discount_bp = 1000 WHERE id = '...';
ALTER TABLE accounts
    ADD COLUMN shop_discount_bp BIGINT NOT NULL DEFAULT 0
    CHECK (shop_discount_bp >= 0 AND shop_discount_bp <= 10000);

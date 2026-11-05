-- M12.5c (PLAN-M12): золото персонажа — стоимость улучшения (ФР-10) и
-- база для сделок (M12.7, trade.set_gold).
ALTER TABLE characters ADD COLUMN gold BIGINT NOT NULL DEFAULT 0;

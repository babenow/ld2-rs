-- M12.5b (PLAN-M12, ADR-002): количество сокетов на определении посоха.
ALTER TABLE item_defs ADD COLUMN sockets INTEGER NOT NULL DEFAULT 0;

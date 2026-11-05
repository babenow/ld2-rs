-- Откат миграции 0007 (сокеты определений, M12.5b).
ALTER TABLE item_defs DROP COLUMN IF EXISTS sockets;

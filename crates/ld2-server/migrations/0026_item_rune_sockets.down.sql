-- Откат миграции 0026 (руна-слоты в stats возвращаются сами собой).
ALTER TABLE item_defs DROP COLUMN IF EXISTS rune_sockets;

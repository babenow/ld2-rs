-- M39: персональные предметы — нельзя передавать, выброс уничтожает.
ALTER TABLE item_defs ADD COLUMN personal BOOLEAN NOT NULL DEFAULT FALSE;

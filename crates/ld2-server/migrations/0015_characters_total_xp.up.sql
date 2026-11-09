-- M15/F24 (окно персонажа): общий опыт за убийства → уровень.
ALTER TABLE characters ADD COLUMN total_xp BIGINT NOT NULL DEFAULT 0;

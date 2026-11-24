-- M28/B2: флаг выдачи стартового набора сумок существующим персонажам.
ALTER TABLE characters ADD COLUMN bags_granted BOOLEAN NOT NULL DEFAULT FALSE;

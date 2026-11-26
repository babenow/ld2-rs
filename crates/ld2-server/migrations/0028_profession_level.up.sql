-- M21: уровень профессии (0..250) — отдельная шкала от ранга
-- (ранг1–5 по границам 50 уровней; план M21).
ALTER TABLE character_professions ADD COLUMN level INT NOT NULL DEFAULT 0;

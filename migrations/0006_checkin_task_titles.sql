-- Preserve the school task title when confirming an uncertain submission later.
ALTER TABLE checkin_records ADD COLUMN task_title text NOT NULL DEFAULT '';

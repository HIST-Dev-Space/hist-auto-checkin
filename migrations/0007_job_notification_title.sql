-- Keep the task name separate from the result for three-line notifications,
-- including executions that reuse an earlier successful check-in record.
ALTER TABLE checkin_jobs ADD COLUMN task_title text NOT NULL DEFAULT '';

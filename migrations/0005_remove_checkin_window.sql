-- Queue time does not limit execution; jobs finish after at most three attempts.
ALTER TABLE checkin_jobs DROP COLUMN deadline_at;
ALTER TABLE checkin_schedules DROP COLUMN retry_window_minutes;

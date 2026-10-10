-- An empty course list discovers the current task from the school at execution time.
-- Existing rules with selected courses continue to work.
ALTER TABLE checkin_schedules
    DROP CONSTRAINT checkin_schedules_course_ids_check;
ALTER TABLE checkin_schedules
    ADD CONSTRAINT checkin_schedules_course_ids_check
    CHECK (cardinality(course_ids) BETWEEN 0 AND 20);

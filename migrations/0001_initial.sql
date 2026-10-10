CREATE TABLE users (
    id uuid PRIMARY KEY,
    username text NOT NULL UNIQUE,
    password_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE sessions (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);
CREATE INDEX sessions_expiry ON sessions(expires_at);
CREATE TABLE auth_buckets (
    bucket_key text PRIMARY KEY,
    hits integer NOT NULL,
    expires_at timestamptz NOT NULL
);
CREATE TABLE school_accounts (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label text NOT NULL,
    student_no text NOT NULL,
    cookies_ciphertext text NOT NULL,
    credential_status text NOT NULL DEFAULT 'active' CHECK (credential_status IN ('active','invalid')),
    execution_token uuid,
    execution_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(user_id, student_no),
    UNIQUE(id, user_id)
);
CREATE TABLE checkin_schedules (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    school_account_id uuid NOT NULL,
    course_ids text[] NOT NULL CHECK (cardinality(course_ids) BETWEEN 1 AND 20),
    local_time time NOT NULL,
    timezone text NOT NULL,
    weekdays integer[] NOT NULL CHECK (cardinality(weekdays) BETWEEN 1 AND 7 AND weekdays <@ ARRAY[1,2,3,4,5,6,7]),
    latitude double precision NOT NULL CHECK (latitude BETWEEN -90 AND 90),
    longitude double precision NOT NULL CHECK (longitude BETWEEN -180 AND 180),
    location_name text NOT NULL,
    retry_window_minutes integer NOT NULL CHECK (retry_window_minutes BETWEEN 1 AND 60),
    enabled boolean NOT NULL DEFAULT true,
    next_run_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (school_account_id, user_id) REFERENCES school_accounts(id, user_id) ON DELETE CASCADE
);
CREATE INDEX schedules_due ON checkin_schedules(next_run_at) WHERE enabled;
CREATE TABLE checkin_jobs (
    id uuid PRIMARY KEY,
    schedule_id uuid NOT NULL REFERENCES checkin_schedules(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    school_account_id uuid NOT NULL REFERENCES school_accounts(id) ON DELETE CASCADE,
    scheduled_at timestamptz NOT NULL,
    deadline_at timestamptz NOT NULL,
    available_at timestamptz NOT NULL,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','succeeded','failed','cancelled')),
    attempts integer NOT NULL DEFAULT 0,
    lease_token uuid,
    lease_until timestamptz,
    result_message text,
    finished_at timestamptz,
    UNIQUE(schedule_id, scheduled_at)
);
CREATE INDEX jobs_due ON checkin_jobs(available_at) WHERE status = 'pending';
CREATE INDEX jobs_running ON checkin_jobs(lease_until) WHERE status = 'running';
CREATE INDEX jobs_user ON checkin_jobs(user_id, scheduled_at DESC);
CREATE TABLE checkin_records (
    school_account_id uuid NOT NULL REFERENCES school_accounts(id) ON DELETE CASCADE,
    course_id text NOT NULL,
    punch_id text NOT NULL,
    punch_url text NOT NULL,
    job_id uuid NOT NULL REFERENCES checkin_jobs(id) ON DELETE CASCADE,
    status text NOT NULL CHECK (status IN ('submitting','uncertain','succeeded','failed')),
    message text NOT NULL,
    latitude double precision NOT NULL,
    longitude double precision NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(school_account_id, course_id, punch_id)
);
CREATE TABLE weixin_bindings (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    bot_id text NOT NULL UNIQUE,
    recipient_id text NOT NULL,
    token_ciphertext text NOT NULL,
    context_ciphertext text,
    context_updated_at timestamptz,
    cursor_ciphertext text,
    base_url text NOT NULL,
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active','expired')),
    poll_token uuid,
    poll_until timestamptz,
    poll_available_at timestamptz NOT NULL DEFAULT now(),
    last_error text,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX weixin_due ON weixin_bindings(poll_available_at) WHERE status = 'active';
CREATE TABLE weixin_login_sessions (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    qrcode_ciphertext text NOT NULL,
    base_url text NOT NULL,
    status text NOT NULL DEFAULT 'wait',
    expires_at timestamptz NOT NULL,
    poll_token uuid,
    poll_until timestamptz
);
CREATE TABLE notification_outbox (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    job_id uuid NOT NULL UNIQUE REFERENCES checkin_jobs(id) ON DELETE CASCADE,
    body text NOT NULL,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','sending','sent','failed')),
    attempts integer NOT NULL DEFAULT 0,
    available_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL DEFAULT now() + interval '1 day',
    lease_token uuid,
    lease_until timestamptz,
    last_error text,
    sent_at timestamptz
);
CREATE INDEX notifications_due ON notification_outbox(available_at) WHERE status = 'pending';

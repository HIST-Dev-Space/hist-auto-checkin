ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;
CREATE TABLE school_identities (
    school_uid text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    school_account_id uuid NOT NULL REFERENCES school_accounts(id) ON DELETE CASCADE,
    UNIQUE(user_id,school_account_id)
);
CREATE TABLE school_login_sessions (
    id uuid PRIMARY KEY,
    secret_hash bytea NOT NULL,
    jar_ciphertext text NOT NULL,
    method text NOT NULL CHECK (method IN ('wechat','identity')),
    expires_at timestamptz NOT NULL,
    lease_token uuid,
    lease_until timestamptz
);
CREATE INDEX school_login_expiry ON school_login_sessions(expires_at);

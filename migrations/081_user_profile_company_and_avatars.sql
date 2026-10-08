-- 081_user_profile_company_and_avatars.sql
--
-- The fleet account/profile surface (programme card t_2cb77960, this app's card t_39cea779).
-- FunnelSwift shipped it on 2026-10-08 (t_ff948669) and MissedCall Respondr followed (t_9cd2c8f2);
-- this app already had a Profile screen that carried only a Name field, so a customer could not
-- record their company and had no picture.
--
-- Three additive objects:
--
--   * `users.username` — the optional handle the fleet profile contract accepts alongside `name`
--     and `company`. Nullable: every existing account has none and must keep working.
--   * `users.company`  — the optional company/organisation the profile screen edits and /auth/me
--     returns. Nullable for the same reason.
--   * `user_avatars`   — ONE row per user holding the uploaded picture's bytes and the content type
--     sniffed from those bytes at upload time.
--
-- WHY THE PICTURE LIVES IN THE DATABASE, NOT A SERVED WEBROOT: this container is IMAGE-BAKED
-- (`bin/deploy-workflowswift.sh` -> `deploy-app.sh workflowswift` rebuilds the image; `docker
-- restart` re-runs the OLD binary and there is NO bind mount for a webroot). A file written at run
-- time would die on the next restart and no host webroot can serve it. The fleet's FunnelSwift
-- reached the same verdict for the same reason and streamed the bytes back through a route; this
-- file is that shape.
--
-- The `user_avatars` row is keyed by `users.id` with ON DELETE CASCADE, so retiring a probe user
-- retires its picture with it and no orphan bytea row can outlive its owner.

ALTER TABLE users ADD COLUMN IF NOT EXISTS username text;
ALTER TABLE users ADD COLUMN IF NOT EXISTS company text;

CREATE TABLE IF NOT EXISTS user_avatars (
    user_id      uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    bytes        bytea NOT NULL,
    content_type text  NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now()
);

-- The content type is only ever the sniffed value for one of four magics (see
-- `auth::handlers::sniff_image`), and an empty body is refused before the write. Pin the four
-- values at the store so a future writer that forgets to sniff FAILS CLOSED instead of storing a
-- row the GET would serve under a guessed type. Guarded by a catalog probe so the re-run on the
-- next boot is a no-op (a bare ADD CONSTRAINT would abort on the second pass).
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'user_avatars_content_type_check'
    ) THEN
        ALTER TABLE user_avatars
            ADD CONSTRAINT user_avatars_content_type_check
            CHECK (content_type IN ('image/png', 'image/jpeg', 'image/gif', 'image/webp'));
    END IF;
END $$;

-- 114_private_email_attachments.sql
-- THE FILE ITSELF (ported to WorkflowSwift from ADASwift's 029, which came from CoreSwift's 147).
--
-- Until this migration both receivers accepted a delivery with attachments, stored the MESSAGE with
-- has_attachments = true, counted the file's bytes into private_email_messages.size_bytes, and then
-- DROPPED the bytes. A customer received an invoice, a screenshot or a signed PDF and the inbox
-- showed a message with nothing to open — silent data loss from their point of view. The app's own
-- signature is an inline `cid:` part, so the same hole was visible on every send: the reader had no
-- image to draw.
--
-- A file part is now its own row, keyed to the message it travelled with and carrying the account it
-- belongs to (so a download can be tenancy-scoped without trusting the message id alone).
--
--   * is_inline = true  — the body refers to it as `cid:<name>`; the reader renders it in place.
--   * is_inline = false — a real attachment; the reader offers it as a download.
--
-- STORAGE IS COUNTED ONCE. The bytes are already inside the parent message's `size_bytes` (that is
-- what the meter, the banner and the 100% refusal read), so nothing sums this table: it is the
-- PAYLOAD, not a second meter. Summing both would double-count every attachment a customer stores.
--
-- ON DELETE CASCADE: destroying a message destroys the files it carried with it — there is nothing
-- left for a download to return, and keeping orphaned bytes would leak storage the owner deleted.
--
-- THIS APP'S OWN VOCABULARY: the account column is `tenant_id` (every private-email table keeps that
-- name) but it REFERENCES `accounts(id)`, because WorkflowSwift renamed the tenant table to accounts.

CREATE TABLE IF NOT EXISTS private_email_attachments (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id   uuid NOT NULL REFERENCES private_email_messages(id) ON DELETE CASCADE,
    -- The name the sender gave the part. Rendered on the download chip and used as the saved
    -- filename; sanitised at the door, never trusted as a path.
    filename     text NOT NULL,
    content_type varchar(255) NOT NULL DEFAULT 'application/octet-stream',
    -- The MIME Content-ID (without angle brackets) when the part is referenced from the body.
    content_id   text,
    is_inline    boolean NOT NULL DEFAULT false,
    size_bytes   integer NOT NULL DEFAULT 0,
    -- The bytes. 25 MB is the receiver's own envelope; a row can never be larger than one delivery.
    data         bytea NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- Every read is "the files of THIS message" through the account's own id.
CREATE INDEX IF NOT EXISTS idx_private_email_attachments_message
    ON private_email_attachments (tenant_id, message_id);
CREATE INDEX IF NOT EXISTS idx_private_email_attachments_inline
    ON private_email_attachments (message_id) WHERE is_inline = true;

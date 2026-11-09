-- Native context is retained separately from one-shot work/call authority.
-- An unfinished newest opening prevents reuse of an older checkpoint.
CREATE TABLE owner_shared_native_turns (
 work_id TEXT PRIMARY KEY REFERENCES owner_shared_work_inbox(id),
 scope_digest TEXT NOT NULL CHECK(length(scope_digest)=64),
 process_digest TEXT NOT NULL CHECK(length(process_digest)=64),
 opened_revision INTEGER NOT NULL UNIQUE CHECK(opened_revision>0 AND opened_revision<=9007199254740991)
) STRICT;
CREATE INDEX owner_shared_native_turn_scope ON owner_shared_native_turns(scope_digest,opened_revision DESC);
CREATE TABLE owner_shared_native_checkpoints (
 work_id TEXT PRIMARY KEY REFERENCES owner_shared_native_turns(work_id),
 native_id TEXT NOT NULL CHECK(length(CAST(native_id AS BLOB)) BETWEEN 1 AND 255),
 receipt_id TEXT NOT NULL UNIQUE REFERENCES owner_shared_work_observations(id)
) STRICT;
CREATE TRIGGER owner_shared_native_turns_immutable BEFORE UPDATE ON owner_shared_native_turns BEGIN SELECT RAISE(ABORT,'immutable native context history'); END;
CREATE TRIGGER owner_shared_native_turns_retained BEFORE DELETE ON owner_shared_native_turns BEGIN SELECT RAISE(ABORT,'retained native context history'); END;
CREATE TRIGGER owner_shared_native_checkpoints_immutable BEFORE UPDATE ON owner_shared_native_checkpoints BEGIN SELECT RAISE(ABORT,'immutable native context history'); END;
CREATE TRIGGER owner_shared_native_checkpoints_retained BEFORE DELETE ON owner_shared_native_checkpoints BEGIN SELECT RAISE(ABORT,'retained native context history'); END;

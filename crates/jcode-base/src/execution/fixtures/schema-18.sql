-- Historical execution schema 18, before child FIFO and monitor indexes.
-- DDL from c2dc31ef44f2407d8eb8823ba0a8c6457a6aacc1 crates/jcode-base/src/execution/store.rs, migrations 1-18.
-- Only data is a synthetic provider receipt namespace. Do not regenerate from current schema.
BEGIN TRANSACTION;
CREATE TABLE acceptance_receipts (
                run_id TEXT PRIMARY KEY REFERENCES runs(id), receipt_path TEXT NOT NULL,
                digest TEXT NOT NULL
            );
CREATE TABLE background_deliveries (
                run_id TEXT PRIMARY KEY REFERENCES runs(id), notify INTEGER NOT NULL,
                wake INTEGER NOT NULL, started_at TEXT NOT NULL,
                notify_state TEXT NOT NULL DEFAULT 'pending', wake_state TEXT NOT NULL DEFAULT 'pending',
                notify_attempt TEXT, wake_attempt TEXT,
                CHECK(notify_state IN ('pending','in_flight','delivered','uncertain')),
                CHECK(wake_state IN ('pending','in_flight','delivered','uncertain'))
            );
CREATE TABLE cleanup_reviews(id TEXT PRIMARY KEY,reader TEXT NOT NULL,confirmation TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('reviewed','confirmed')));
CREATE TABLE command_handoffs (
                run_id TEXT PRIMARY KEY REFERENCES runs(id), parent_owner TEXT NOT NULL,
                worker_owner TEXT REFERENCES runtimes(id), payload_digest TEXT NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('prepared','claimed','registered','executing','finished')),
                process_identity TEXT, exec_authorized INTEGER NOT NULL DEFAULT 0
            );
CREATE TABLE inspection_blob_refs(
                snapshot_id TEXT NOT NULL REFERENCES inspection_snapshots(id),
                digest TEXT NOT NULL REFERENCES inspection_blobs(digest),
                PRIMARY KEY(snapshot_id,digest)
            );
CREATE TABLE inspection_blobs(digest TEXT PRIMARY KEY,bytes INTEGER NOT NULL);
CREATE TABLE inspection_output_refs(snapshot_id TEXT NOT NULL REFERENCES inspection_snapshots(id),run_id TEXT NOT NULL REFERENCES runs(id),PRIMARY KEY(snapshot_id,run_id));
CREATE TABLE inspection_snapshots(
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,
                reader TEXT NOT NULL,target TEXT NOT NULL,created INTEGER NOT NULL,
                manifest_digest TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('retained','pruned'))
            );
CREATE TABLE native_processes (
                ticket TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id),
                owner TEXT NOT NULL, pid INTEGER, identity TEXT, identity_error TEXT,
                finished INTEGER NOT NULL DEFAULT 0
            );
CREATE TABLE output_allocations (
                id TEXT PRIMARY KEY REFERENCES runs(id), physical TEXT NOT NULL,
                archived INTEGER NOT NULL, owner TEXT NOT NULL,
                stage TEXT NOT NULL CHECK(stage IN ('prepared','published'))
            , archive_spec TEXT);
CREATE TABLE output_chunks (
                run_id TEXT NOT NULL REFERENCES runs(id), start_byte INTEGER NOT NULL,
                end_byte INTEGER NOT NULL, sha256 TEXT NOT NULL,
                PRIMARY KEY(run_id,start_byte), CHECK(end_byte>start_byte)
            );
CREATE TABLE output_deletions(id TEXT PRIMARY KEY REFERENCES runs(id),review_id TEXT NOT NULL REFERENCES cleanup_reviews(id),stage TEXT NOT NULL CHECK(stage IN ('deleting','deleted')),error TEXT);
CREATE TABLE output_locations (
                id TEXT PRIMARY KEY REFERENCES runs(id), physical TEXT NOT NULL,
                archived INTEGER NOT NULL, generation INTEGER NOT NULL DEFAULT 0,
                capture_error TEXT
            , archive_spec TEXT, cold_archived_at INTEGER);
CREATE TABLE provider_receipt_namespace (id INTEGER PRIMARY KEY CHECK(id=1), namespace TEXT NOT NULL);
INSERT INTO "provider_receipt_namespace" VALUES(1,'11111111111111111111111111111111');
CREATE TABLE provider_result_receipts (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT UNIQUE NOT NULL REFERENCES runs(id),
                session_id TEXT NOT NULL, request_id TEXT NOT NULL,
                request_lease TEXT NOT NULL, tool_use_id TEXT NOT NULL,
                message_id TEXT
            );
CREATE TABLE relocations (
                    id TEXT PRIMARY KEY REFERENCES runs(id), source TEXT NOT NULL,
                    destination TEXT NOT NULL, stage TEXT NOT NULL,
                    manifest_path TEXT NOT NULL
                );
CREATE TABLE runs (
                    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, message_id TEXT NOT NULL,
                    tool TEXT NOT NULL, input_digest TEXT NOT NULL, state TEXT NOT NULL,
                    owner TEXT NOT NULL, input_path TEXT NOT NULL, result_path TEXT,
                    output_path TEXT, output_bytes INTEGER NOT NULL DEFAULT 0,
                    complete INTEGER NOT NULL DEFAULT 0,
                    created INTEGER NOT NULL DEFAULT (unixepoch()),
                    updated INTEGER NOT NULL DEFAULT (unixepoch()), background INTEGER NOT NULL DEFAULT 0, stop_cause TEXT, parent_id TEXT REFERENCES runs(id), progress TEXT, process_exit TEXT, superseded INTEGER NOT NULL DEFAULT 0, native_tracking INTEGER,
                    CHECK (state IN ('prepared','running','completed','failed','cancelled','interrupted'))
                );
CREATE TABLE runtimes (
                id TEXT PRIMARY KEY, endpoint TEXT NOT NULL, auth_key TEXT NOT NULL,
                lease_path TEXT NOT NULL, protocol_version INTEGER NOT NULL, process_id INTEGER NOT NULL
            , process_image TEXT);
CREATE TABLE session_activity (
                session_id TEXT PRIMARY KEY, last_active INTEGER NOT NULL, generation INTEGER NOT NULL
            );
CREATE TABLE session_activity_leases(token TEXT PRIMARY KEY,session_id TEXT NOT NULL,stopped_observed_at INTEGER);
CREATE INDEX runs_session_page ON runs(session_id, id);
CREATE INDEX runs_state_page ON runs(state, created, id);
CREATE INDEX provider_receipts_session ON provider_result_receipts(session_id,sequence);
CREATE INDEX native_processes_run ON native_processes(run_id,finished);
CREATE INDEX session_activity_idle ON session_activity(last_active,session_id);
CREATE INDEX activity_leases_session ON session_activity_leases(session_id);
CREATE INDEX inspection_reader_target ON inspection_snapshots(reader,target,state,created,sequence);
CREATE INDEX inspection_blob_owners ON inspection_blob_refs(digest);
CREATE INDEX inspection_output_owners ON inspection_output_refs(run_id);
CREATE TRIGGER activity_run_created AFTER INSERT ON runs BEGIN
                INSERT INTO session_activity(session_id,last_active,generation) VALUES (NEW.session_id,NEW.created,1)
                ON CONFLICT(session_id) DO UPDATE SET last_active=MAX(session_activity.last_active,excluded.last_active),generation=session_activity.generation+1;
            END;
CREATE TRIGGER activity_run_transition AFTER UPDATE OF state ON runs WHEN NEW.state<>OLD.state BEGIN
                INSERT INTO session_activity(session_id,last_active,generation) VALUES (NEW.session_id,NEW.updated,1)
                ON CONFLICT(session_id) DO UPDATE SET last_active=MAX(session_activity.last_active,excluded.last_active),generation=session_activity.generation+1;
            END;
DELETE FROM "sqlite_sequence";
COMMIT;
PRAGMA user_version=18;

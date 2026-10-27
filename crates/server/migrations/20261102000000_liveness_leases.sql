-- Crawl workers (ARCHITECTURE.md D98, docs/WORKERS.md): seed-check leases. Whoever takes due
-- infohashes (a worker, or the server's own checker under the holder '') moves their
-- `liveness_schedule.due_at` to the lease's end and records the holder here, in one
-- transaction under an advisory lock, so no two holders get the same infohash. A result is
-- accepted from a worker only while it still holds the row; writing any result deletes the
-- row. An unanswered lease needs no cleanup to come back: its `due_at` is the lease's end.
-- No foreign key: a torrent deleted meanwhile only leaves a row that the hourly maintenance
-- removes once it is a day past its end.
CREATE TABLE liveness_leases (
    infohash   text COLLATE "C" PRIMARY KEY,
    worker     text NOT NULL,
    leased_at  bigint NOT NULL,
    expires_at bigint NOT NULL
);
CREATE INDEX liveness_leases_worker ON liveness_leases (worker, expires_at);

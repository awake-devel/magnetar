-- The due query reads `liveness_schedule` since D45, so nothing uses this index any more; it
-- only cost an index write per check and kept updates of `liveness` from being HOT.
DROP INDEX IF EXISTS liveness_expires;

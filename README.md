# PhoenixDB

This project contains my personally built relational Database called PhoenixDB.
It is mainly inspired by the SQLite database with some changes.

---

Sources I used:
- https://github.com/sqlite/sqlite
- https://cstack.github.io/db_tutorial/
- https://www.youtube.com/watch?v=5Pc18ge9ohI
- Scalable Datamangement Systems Course at TU Darmstadt

---


## Running

```sh
# Server (creates phoenix.db / phoenix.wal / phoenix.catalog in cwd)
cargo run -p phoenix-server -- --port 7878 --db phoenix.db

# Client REPL
cargo run -p phoenix-client -- --host 127.0.0.1 --port 7878
```

Supported SQL (one statement per line):

```sql
CREATE TABLE users (id INT, name VARCHAR);
INSERT INTO users VALUES (1, 'alice');
SELECT id, name FROM users WHERE id + 1 <= 5 AND name <> 'bob' ORDER BY id DESC;
BEGIN;            
INSERT INTO users VALUES (2, 'bob');
COMMIT;           -- or ROLLBACK
```

## Durability model

- Transactions are buffered per connection and applied at `COMMIT`
  (deferred update), so `ROLLBACK` simply discards the buffer.
- At commit, the after-images of all dirtied pages plus a catalog snapshot
  are appended to the WAL and fsynced before any of those pages may
  reach the data file (write-ahead + no-steal buffer pool).
- On startup the WAL is replayed: only page images sealed by an intact,
  CRC-checked commit record are applied. A torn tail is ignored, which
  rolls back the unfinished transaction.
- A checkpoint (flush all pages + fsync + save catalog + truncate WAL)
  runs automatically once the WAL passes a size threshold, or via
  `Engine::checkpoint()` for a clean shutdown. Killing the process is
  always safe — the next start recovers from the WAL.

Known simplifications (on purpose, for now):
- Rows are fixed-size (256 bytes) with an auto-assigned rowid key.
- A `SELECT` inside an open transaction does not see that transaction's
  own uncommitted writes.
- `CREATE TABLE` is auto-commit only (not allowed inside `BEGIN ... COMMIT`).
- The disk manager's free list is in-memory only; freed pages leak after
  a restart (no corruption, just unused space).

## Tests

Test can be run via the command

```sh
cargo test --workspace
```


"""Durable receipts. Model output is an input to replay, never recomputed on replay."""
import json
import sqlite3
import threading
import time
from pathlib import Path


def encode(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, allow_nan=False)


class Store:
    def __init__(self, path):
        self.path = Path(path)
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        self.lock = threading.RLock()
        self.db = sqlite3.connect(path, check_same_thread=False)
        self.db.row_factory = sqlite3.Row
        self.db.executescript("""
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS objects(kind TEXT, id TEXT, data TEXT,
                PRIMARY KEY(kind,id));
            CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,
                trader TEXT, time REAL, kind TEXT, data TEXT);
            CREATE TABLE IF NOT EXISTS calls(trader TEXT, id TEXT, name TEXT,
                args TEXT, status TEXT, result TEXT, PRIMARY KEY(trader,id));
        """)

    def get(self, kind, key, default=None):
        with self.lock:
            row = self.db.execute("SELECT data FROM objects WHERE kind=? AND id=?", (kind, key)).fetchone()
            return json.loads(row[0]) if row else default

    def put(self, kind, key, value):
        with self.lock, self.db:
            self.db.execute("INSERT OR REPLACE INTO objects VALUES(?,?,?)", (kind, key, encode(value)))

    def all(self, kind):
        with self.lock:
            return [json.loads(r[0]) for r in self.db.execute("SELECT data FROM objects WHERE kind=? ORDER BY id", (kind,))]

    def event(self, trader, kind, data):
        with self.lock, self.db:
            self.db.execute("INSERT INTO events(trader,time,kind,data) VALUES(?,?,?,?)", (trader, time.time(), kind, encode(data)))

    def events(self, trader, after=0, limit=200, tail=False, until=2**63-1):
        with self.lock:
            rows = self.db.execute(
                "SELECT * FROM events WHERE trader=? AND seq>? AND seq<=? ORDER BY seq " + ("DESC" if tail else "ASC") + " LIMIT ?",
                (trader, after, until, min(limit, 500))).fetchall()
            if tail:
                rows.reverse()
            return [dict(r) | {"data": json.loads(r["data"])} for r in rows]

    def reserve(self, trader, key, name, args):
        payload = encode(args)
        with self.lock, self.db:
            row = self.db.execute("SELECT * FROM calls WHERE trader=? AND id=?", (trader, key)).fetchone()
            if row:
                if row["name"] != name or row["args"] != payload:
                    raise ValueError("request id reused with different arguments")
                return dict(row)
            self.db.execute("INSERT INTO calls VALUES(?,?,?,?,?,NULL)", (trader, key, name, payload, "pending"))
            if name == "trade":
                self.db.execute("INSERT OR REPLACE INTO objects VALUES(?,?,?)", ("exchange_reservation", f"{trader}:{key}", "true"))
            return None

    def last_event(self, trader, kind):
        with self.lock:
            row = self.db.execute("SELECT * FROM events WHERE trader=? AND kind=? ORDER BY seq DESC LIMIT 1", (trader, kind)).fetchone()
            return dict(row) | {"data": json.loads(row["data"])} if row else None

    def finish(self, trader, key, result):
        with self.lock, self.db:
            self.db.execute("UPDATE calls SET status='done',result=? WHERE trader=? AND id=?", (encode(result), trader, key))

    def pending(self, trader):
        with self.lock:
            return [dict(r) for r in self.db.execute("SELECT * FROM calls WHERE trader=? AND status='pending' ORDER BY rowid", (trader,))]

    def receipt(self, trader, key):
        with self.lock:
            row = self.db.execute("SELECT status,result FROM calls WHERE trader=? AND id=?", (trader, key)).fetchone()
            return {"status": row["status"], "result": json.loads(row["result"]) if row["result"] else None} if row else {"status": "not_submitted"}

    def call_record(self, trader, key):
        with self.lock:
            row = self.db.execute("SELECT * FROM calls WHERE trader=? AND id=?", (trader, key)).fetchone()
            return dict(row) if row else None

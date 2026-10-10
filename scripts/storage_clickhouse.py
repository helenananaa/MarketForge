#!/usr/bin/env python3
"""Batch verified CLOSED-room archives into ClickHouse and verify every payload.

This is a historical query mirror, not a live CDC or a transactional write path.
Retries reuse immutable archive identities; readers use FINAL for deduplication.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
from urllib.parse import urlencode, urlsplit
from urllib.request import Request, urlopen

from storage_archive import encoded, read_rows, verify_archive

TABLE = "marketforge_room_archive_rows"


def ch_literal(value):
    return "'" + str(value).replace("\\", "\\\\").replace("'", "\\'") + "'"


class ClickHouse:
    def __init__(self, database="default"):
        self.url = os.environ.get("MARKETFORGE_CLICKHOUSE_URL", "http://127.0.0.1:8123")
        parsed = urlsplit(self.url)
        if parsed.scheme not in ("http", "https") or parsed.username or parsed.password or parsed.query or parsed.fragment:
            raise ValueError("use an HTTP(S) endpoint without credentials or query parameters")
        if not re.fullmatch(r"[a-zA-Z_][a-zA-Z0-9_]*", database):
            raise ValueError("invalid ClickHouse database name")
        self.database = database

    def query(self, sql, payload=b""):
        params = {"database": self.database, "query": sql, "wait_end_of_query": "1", "output_format_json_quote_64bit_integers": "0"}
        headers = {"Content-Type": "application/octet-stream"}
        for name, header in (("MARKETFORGE_CLICKHOUSE_USER", "X-ClickHouse-User"), ("MARKETFORGE_CLICKHOUSE_PASSWORD", "X-ClickHouse-Key")):
            if name in os.environ:
                headers[header] = os.environ[name]
        return urlopen(Request(self.url.rstrip("/") + "/?" + urlencode(params), data=payload,
                               headers=headers, method="POST"), timeout=120)

    def execute(self, sql, payload=b""):
        with self.query(sql, payload) as response:
            result = response.read()
            if response.headers.get("X-ClickHouse-Exception-Code"):
                raise RuntimeError("ClickHouse reported a query failure")
            return result

    def initialize(self):
        self.execute(f"""CREATE TABLE IF NOT EXISTS {TABLE} (
          archive_id String, room_id String, table_name LowCardinality(String),
          row_ordinal UInt64, command_seq Nullable(UInt64), event_seq Nullable(UInt64),
          market_time_ms Nullable(UInt64), instrument_id LowCardinality(String),
          event_type LowCardinality(String), price_tick Nullable(Int64), qty Nullable(UInt64),
          payload String
        ) ENGINE=ReplacingMergeTree
        ORDER BY (room_id, table_name, archive_id, row_ordinal)"""
        )


def verify_mirror(directory, client):
    local = verify_archive(directory)
    manifest = json.loads((Path(directory) / "manifest.json").read_bytes())
    archive = ch_literal(manifest["archive_id"])
    room = ch_literal(manifest["room_id"])
    for table in manifest["tables"]:
        digest = hashlib.sha256()
        count = size = 0
        sql = f"SELECT row_ordinal,payload FROM {TABLE} FINAL WHERE room_id={room} AND archive_id={archive} AND table_name={ch_literal(table['table'])} ORDER BY row_ordinal FORMAT JSONEachRow"
        with client.query(sql) as response:
            for line in response:
                row = json.loads(line)
                if int(row["row_ordinal"]) != count:
                    raise ValueError("ClickHouse mirror has a missing or duplicate ordinal")
                data = (row["payload"] + "\n").encode()
                digest.update(data)
                count += 1
                size += len(data)
        if (count, size, digest.hexdigest()) != (table["rows"], table["raw_bytes"], table["sha256"]):
            raise ValueError(f"ClickHouse payload verification failed: {table['table']}")
    return {**local, "clickhouse_verified": True}


def import_archive(directory, client, batch_rows=1000):
    verify_archive(directory)
    manifest = json.loads((Path(directory) / "manifest.json").read_bytes())
    if manifest["status"] != "closed":
        raise ValueError("historical mirror accepts only closed-room archives")
    client.initialize()
    room = ch_literal(manifest["room_id"])
    archive = ch_literal(manifest["archive_id"])
    # A second archive version of this room must not double its historical events.
    existing = client.execute(f"SELECT count() FROM {TABLE} WHERE room_id={room} AND archive_id!={archive}").strip()
    if int(existing):
        raise ValueError("room already has a different archive version; reconcile it before importing")
    batch = []
    for table in manifest["tables"]:
        for ordinal, row in enumerate(read_rows(directory, table)):
            batch.append(encoded({"archive_id": manifest["archive_id"], "room_id": manifest["room_id"],
                "table_name": table["table"], "row_ordinal": ordinal,
                "command_seq": row.get("command_seq"), "event_seq": row.get("event_seq"),
                "market_time_ms": row.get("market_time_ms"), "instrument_id": row.get("instrument_id") or "",
                "event_type": row.get("event_type") or row.get("mutation_kind") or "",
                "price_tick": row.get("price_tick"), "qty": row.get("qty"),
                "payload": encoded(row).decode().rstrip("\n")}))
            if len(batch) >= batch_rows:
                client.execute(f"INSERT INTO {TABLE} FORMAT JSONEachRow", b"".join(batch))
                batch.clear()
    if batch:
        client.execute(f"INSERT INTO {TABLE} FORMAT JSONEachRow", b"".join(batch))
    return verify_mirror(directory, client)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("import", "verify"))
    parser.add_argument("directory", type=Path)
    parser.add_argument("--database", default="default")
    parser.add_argument("--batch-rows", type=int, default=1000)
    args = parser.parse_args()
    if not 1 <= args.batch_rows <= 100000:
        parser.error("batch rows must be 1..100000")
    client = ClickHouse(args.database)
    result = import_archive(args.directory, client, args.batch_rows) if args.command == "import" else verify_mirror(args.directory, client)
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()

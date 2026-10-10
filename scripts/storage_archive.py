#!/usr/bin/env python3
"""Consistent, lossless room archives. Only Python stdlib and psql are required.

Archives never delete source rows. Restore is restricted to an empty, migrated
database and requires an explicit --isolated-target acknowledgement.
"""
from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from urllib.parse import parse_qsl, unquote, urlsplit

FORMAT = "marketforge.room-archive.v1"


def encoded(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def literal(value):
    return "'" + str(value).replace("'", "''") + "'"


def identifier(value):
    if not re.fullmatch(r"marketforge_[a-z_]+", value):
        raise ValueError("invalid archive table name")
    return 'public."' + value + '"'


def pg_environment(env_name):
    dsn = os.environ.get(env_name)
    if not dsn:
        raise ValueError(f"set {env_name} to an isolated database connection string")
    env = os.environ.copy()
    mapping = {"host": "PGHOST", "hostaddr": "PGHOSTADDR", "port": "PGPORT", "user": "PGUSER",
               "password": "PGPASSWORD", "dbname": "PGDATABASE", "sslmode": "PGSSLMODE",
               "sslrootcert": "PGSSLROOTCERT", "sslcert": "PGSSLCERT", "sslkey": "PGSSLKEY",
               "connect_timeout": "PGCONNECT_TIMEOUT", "options": "PGOPTIONS", "application_name": "PGAPPNAME"}
    if dsn.startswith(("postgres://", "postgresql://")):
        uri = urlsplit(dsn)
        params = {"host": uri.hostname or "localhost", "port": str(uri.port or 5432),
                  "dbname": unquote(uri.path.lstrip("/"))}
        if uri.username is not None:
            params["user"] = unquote(uri.username)
        if uri.password is not None:
            params["password"] = unquote(uri.password)
        params.update(parse_qsl(uri.query))
    else:
        params = dict(part.split("=", 1) for part in shlex.split(dsn))
    for key, value in params.items():
        if key not in mapping:
            raise ValueError("unsupported PostgreSQL connection option")
        env[mapping[key]] = value
    # Do not put credentials in process arguments or diagnostic messages.
    return env


def pg_process(sql, env_name):
    proc = subprocess.Popen(
        ["psql", "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-v", "FETCH_COUNT=10000"],
        env=pg_environment(env_name), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return proc


def pg_json(sql, env_name):
    proc = pg_process(sql, env_name)
    out, err = proc.communicate(sql.encode(), timeout=120)
    if proc.returncode:
        raise RuntimeError("PostgreSQL query failed (connection details withheld): " + err.decode().splitlines()[-1])
    return [json.loads(line) for line in out.splitlines() if line]


def catalog(env_name):
    return pg_json("""
    SELECT jsonb_build_object('table', c.relname, 'columns',
      (SELECT jsonb_agg(a.attname ORDER BY a.attnum) FROM pg_attribute a
       WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
      'primary_key', (SELECT jsonb_agg(a.attname ORDER BY k.ordinality)
       FROM pg_index i CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum, ordinality)
       JOIN pg_attribute a ON a.attrelid=c.oid AND a.attnum=k.attnum
       WHERE i.indrelid=c.oid AND i.indisprimary))
    FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
    WHERE n.nspname='public' AND c.relkind IN ('r','p')
      AND c.relname LIKE 'marketforge\\_%' ESCAPE '\\'
    ORDER BY c.relname;
    """, env_name)


def export_room(room_id, output, env_name, allow_active=False):
    output = Path(output)
    if output.exists():
        raise ValueError("archive output already exists")
    tables = catalog(env_name)
    if not tables:
        raise ValueError("database has no MarketForge schema")
    selected = []
    statements = ["BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;", "SET LOCAL standard_conforming_strings=on;"]
    room = literal(room_id)
    for table in tables:
        name = table["table"]
        identifier(name)
        if "room_id" in table["columns"]:
            predicate = f"t.room_id={room}"
        elif name == "marketforge_users":
            predicate = f"EXISTS (SELECT 1 FROM marketforge_room_members m WHERE m.room_id={room} AND m.user_id=t.user_id)"
        elif name == "marketforge_schema_migrations":
            predicate = "true"
        else:
            raise ValueError(f"unrecognized global table {name}; archive scope must be reviewed")
        keys = table["primary_key"]
        if not keys:
            raise ValueError(f"table {name} has no primary key")
        order = ",".join('t."' + key.replace('"', '""') + '"' for key in keys)
        statements.append(f"SELECT jsonb_build_object('table',{literal(name)},'row',to_jsonb(t)) FROM {identifier(name)} t WHERE {predicate} ORDER BY {order};")
        selected.append(table)
    statements.append("COMMIT;")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix=".room-archive-", dir=output.parent))
    stats = {}
    streams = {}
    hashes = {}
    proc = None
    try:
        for table in selected:
            name = table["table"]
            streams[name] = gzip.GzipFile(filename=str(temporary / (name + ".jsonl.gz")), mode="wb", mtime=0)
            hashes[name] = hashlib.sha256()
            stats[name] = {**table, "file": name + ".jsonl.gz", "rows": 0, "raw_bytes": 0}
        proc = pg_process("", env_name)
        proc.stdin.write(("\n".join(statements) + "\n").encode())
        proc.stdin.close()
        for line in proc.stdout:
            item = json.loads(line)
            name = item["table"]
            data = encoded(item["row"])
            streams[name].write(data)
            hashes[name].update(data)
            stats[name]["rows"] += 1
            stats[name]["raw_bytes"] += len(data)
        err = proc.stderr.read()
        if proc.wait():
            raise RuntimeError("archive query failed: " + err.decode().splitlines()[-1])
        for stream in streams.values():
            stream.close()
        if stats["marketforge_rooms"]["rows"] != 1:
            raise ValueError("room does not exist")
        room_row = next(read_rows(temporary, stats["marketforge_rooms"]))
        if room_row["status"] != "closed" and not allow_active:
            raise ValueError("room is active; close it or use --allow-active for a point-in-time archive")
        for name, table in stats.items():
            table["sha256"] = hashes[name].hexdigest()
            table["compressed_bytes"] = (temporary / table["file"]).stat().st_size
            table["compressed_sha256"] = file_hash(temporary / table["file"])
        manifest = {
            "format": FORMAT, "room_id": room_id, "status": room_row["status"],
            "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "tables": list(stats.values()), "source_deleted": False,
        }
        manifest["archive_id"] = archive_id(manifest)
        (temporary / "manifest.json").write_bytes(encoded(manifest))
        verify_archive(temporary)
        temporary.rename(output)
        return manifest
    except BaseException:
        if proc is not None and proc.poll() is None:
            proc.kill()
            proc.wait()
        for stream in streams.values():
            stream.close()
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def file_hash(path):
    result = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def archive_id(manifest):
    return hashlib.sha256(encoded({"format": manifest["format"], "room_id": manifest["room_id"],
                                  "tables": manifest["tables"]})).hexdigest()


def read_rows(directory, table):
    expected = table["table"] + ".jsonl.gz"
    identifier(table["table"])
    if table["file"] != expected:
        raise ValueError("invalid archive file path")
    with gzip.open(Path(directory) / expected, "rt", encoding="utf-8") as stream:
        for line in stream:
            yield json.loads(line)


def verify_archive(directory):
    directory = Path(directory)
    manifest = json.loads((directory / "manifest.json").read_bytes())
    if manifest["format"] != FORMAT or archive_id(manifest) != manifest["archive_id"]:
        raise ValueError("invalid archive manifest or digest")
    names = [table["table"] for table in manifest["tables"]]
    if len(names) != len(set(names)) or not {"marketforge_rooms", "marketforge_executions", "marketforge_room_mutations"}.issubset(names):
        raise ValueError("archive has duplicate or missing canonical tables")
    boundaries = {}
    for table in manifest["tables"]:
        name = table["table"]
        identifier(name)
        if table["file"] != name + ".jsonl.gz":
            raise ValueError("invalid archive file path")
        path = directory / table["file"]
        if path.stat().st_size != table["compressed_bytes"] or file_hash(path) != table["compressed_sha256"]:
            raise ValueError(f"compressed checksum mismatch: {name}")
        digest = hashlib.sha256()
        rows = size = 0
        previous = None
        first_seq = last_seq = None
        for row in read_rows(directory, table):
            if "room_id" in table["columns"] and row.get("room_id") != manifest["room_id"]:
                raise ValueError("archive row crosses room scope")
            if set(row) != set(table["columns"]):
                raise ValueError("archive row schema differs from manifest")
            key = tuple(row[k] for k in table["primary_key"])
            if previous is not None and key <= previous:
                raise ValueError(f"duplicate or unordered primary key: {name}")
            previous = key
            if name == "marketforge_executions":
                seq = row["command_seq"]
                if last_seq is not None and seq != last_seq + 1:
                    raise ValueError("execution archive contains a sequence gap")
                if first_seq is None:
                    first_seq = seq
                last_seq = seq
            data = encoded(row)
            digest.update(data)
            size += len(data)
            rows += 1
        if (rows, size, digest.hexdigest()) != (table["rows"], table["raw_bytes"], table["sha256"]):
            raise ValueError(f"uncompressed checksum/count mismatch: {name}")
        if name == "marketforge_executions":
            if first_seq not in (None, 0):
                raise ValueError("full archive must start at command sequence 0")
            boundaries = {"first_command_seq": first_seq, "last_command_seq": last_seq}
    return {"archive_id": manifest["archive_id"], "room_id": manifest["room_id"],
            "tables": len(names), "rows": sum(t["rows"] for t in manifest["tables"]), **boundaries}


def recovery_json(directory):
    verify_archive(directory)
    directory = Path(directory)
    manifest = json.loads((directory / "manifest.json").read_bytes())
    tables = {t["table"]: t for t in manifest["tables"]}
    rooms = [{"room_id": r["room_id"], "scenario": r["scenario_json"], "status": r["status"].capitalize()}
             for r in read_rows(directory, tables["marketforge_rooms"])]
    executions = []
    for row in read_rows(directory, tables["marketforge_executions"]):
        executions.append({"room_id": row["room_id"], "command_seq": row["command_seq"],
                           "participant_id": row["participant_id"], "account_id": int(row["account_id"]) if row["account_id"] is not None else None,
                           "request_user_id": row.get("request_user_id"), "idempotency_key": row.get("idempotency_key"),
                           "request_fingerprint": row.get("request_fingerprint"), "command": row["command_json"], "execution": row["execution_json"]})
    mutations = [{"room_id": r["room_id"], "mutation_seq": r["mutation_seq"], "command_cursor": r["command_cursor"],
                  "schema_version": r["schema_version"], "mutation": r["payload_json"]}
                 for r in read_rows(directory, tables["marketforge_room_mutations"])]
    snapshots = []
    if "marketforge_room_snapshots" in tables:
        rows = list(read_rows(directory, tables["marketforge_room_snapshots"]))
        if rows:
            row = rows[-1]
            snapshots = [{"room_id": row["room_id"], "command_seq": row["command_seq"], "actor": row["actor_json"]}]
    return {"rooms": rooms, "executions": executions, "mutations": mutations, "snapshots": snapshots}


def restore_archive(directory, env_name):
    verify_archive(directory)
    directory = Path(directory)
    manifest = json.loads((directory / "manifest.json").read_bytes())
    source = {t["table"]: t for t in manifest["tables"]}
    target = {t["table"]: t for t in catalog(env_name)}
    if set(source) != set(target) or any(source[k]["columns"] != target[k]["columns"] for k in source):
        raise ValueError("target schema differs; migrate an empty database to the archive schema first")
    source_versions = [(r["version"], r["name"]) for r in read_rows(directory, source["marketforge_schema_migrations"])]
    target_versions = pg_json("SELECT jsonb_build_array(version,name) FROM marketforge_schema_migrations ORDER BY version;", env_name)
    if source_versions != [tuple(r) for r in target_versions]:
        raise ValueError("target migration versions differ from archive")
    # Stream SQL through a file; all inserts and sequence repair commit atomically.
    with tempfile.TemporaryFile() as script:
        script.write(b"BEGIN; SET LOCAL standard_conforming_strings=on;\n")
        namespace = int.from_bytes(b"MKTF", "big")
        runtime_lock = int.from_bytes(b"RUN1", "big")
        script.write(f"DO $$ BEGIN IF NOT pg_try_advisory_xact_lock({namespace},{runtime_lock}) THEN RAISE EXCEPTION 'stop the target runtime before archive restore'; END IF; END $$;\n".encode())
        script.write(b"LOCK TABLE marketforge_rooms,marketforge_users IN ACCESS EXCLUSIVE MODE;\n")
        # Migration 6 seeds local-user even in an otherwise empty database.
        script.write(b"DO $$ BEGIN IF EXISTS (SELECT 1 FROM marketforge_rooms) OR EXISTS (SELECT 1 FROM marketforge_users WHERE user_id<>'local-user') THEN RAISE EXCEPTION 'restore requires an empty isolated database'; END IF; END $$;\n")
        names = ["marketforge_users", "marketforge_rooms"] + sorted(set(source) - {"marketforge_users", "marketforge_rooms", "marketforge_schema_migrations"})
        for name in names:
            for row in read_rows(directory, source[name]):
                payload = json.dumps(row, separators=(",", ":"), ensure_ascii=False)
                conflict = " ON CONFLICT (user_id) DO UPDATE SET created_at=EXCLUDED.created_at" if name == "marketforge_users" else ""
                script.write(f"INSERT INTO {identifier(name)} SELECT (jsonb_populate_record(NULL::{identifier(name)},{literal(payload)}::jsonb)).*{conflict};\n".encode())
        # Restored leases must never advertise ownership of the original runtime.
        script.write(b"UPDATE marketforge_room_writer_leases SET lease_expires_at=clock_timestamp();\n")
        script.write(b"SELECT setval(pg_get_serial_sequence('marketforge_room_mutations','mutation_seq'), COALESCE((SELECT MAX(mutation_seq) FROM marketforge_room_mutations),1), EXISTS(SELECT 1 FROM marketforge_room_mutations));\nCOMMIT;\n")
        script.seek(0)
        result = subprocess.run(["psql", "-X", "-q", "-v", "ON_ERROR_STOP=1"], env=pg_environment(env_name), stdin=script, capture_output=True, timeout=300)
        if result.returncode:
            raise RuntimeError("isolated restore failed: " + result.stderr.decode().splitlines()[-1])
    return {"restored_room": manifest["room_id"], "archive_id": manifest["archive_id"], "source_deleted": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dsn-env", default="MARKETFORGE_DATABASE_URL")
    commands = parser.add_subparsers(dest="command", required=True)
    export = commands.add_parser("export")
    export.add_argument("room_id")
    export.add_argument("output", type=Path)
    export.add_argument("--allow-active", action="store_true")
    for name in ("verify", "recovery", "restore"):
        command = commands.add_parser(name)
        command.add_argument("directory", type=Path)
        if name == "restore":
            command.add_argument("--isolated-target", action="store_true", required=True)
    args = parser.parse_args()
    try:
        if args.command == "export":
            result = export_room(args.room_id, args.output, args.dsn_env, args.allow_active)
            result = {k: result[k] for k in ("archive_id", "room_id", "status", "source_deleted")}
        elif args.command == "verify":
            result = verify_archive(args.directory)
        elif args.command == "recovery":
            result = recovery_json(args.directory)
        else:
            result = restore_archive(args.directory, args.dsn_env)
        sys.stdout.buffer.write(encoded(result))
    except (ValueError, RuntimeError, OSError, KeyError, subprocess.TimeoutExpired) as error:
        print(f"archive failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

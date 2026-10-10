#!/usr/bin/env python3
"""Verify archive replay, isolated restore and retry-safe ClickHouse mirroring.

The target must be an empty database already migrated by exchange-server startup.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

from storage_archive import encoded, recovery_json, restore_archive, verify_archive
from storage_clickhouse import ClickHouse, import_archive, verify_mirror

ROOT=Path(__file__).resolve().parents[1]


def audit(mode,room=None,payload=None,env=None):
    command=[str(ROOT/"target/debug/journal-audit"),mode]
    if room is not None:
        command.append(room)
    result=subprocess.run(command,input=payload,capture_output=True,env=env,timeout=300,check=True)
    return json.loads(result.stdout)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive",type=Path)
    parser.add_argument("--restore-dsn-env",default="MARKETFORGE_RESTORE_DATABASE_URL")
    parser.add_argument("--isolated-target",action="store_true",required=True)
    parser.add_argument("--clickhouse",action="store_true")
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    verified=verify_archive(args.archive)
    source=audit("database",verified["room_id"])
    archive=audit("stdin",payload=encoded(recovery_json(args.archive)))
    if source["state"]!=archive["state"]:
        raise ValueError("archive full replay differs from source database")
    restored=restore_archive(args.archive,args.restore_dsn_env)
    target_env=os.environ.copy()
    target_env["MARKETFORGE_DATABASE_URL"]=os.environ[args.restore_dsn_env]
    target=audit("database",verified["room_id"],env=target_env)
    runtime=audit("runtime",verified["room_id"],env=target_env)
    if source["state"]!=target["state"] or source["state"]!=runtime["state"]:
        raise ValueError("restored database/checkpoint differs from source full replay")
    # Source remains untouched, and a second restore must fail instead of merging.
    try:
        restore_archive(args.archive,args.restore_dsn_env)
    except RuntimeError:
        refused=True
    else:
        raise ValueError("restore unexpectedly accepted a nonempty target")
    report={**verified,"archive_full_replay_equal":True,"isolated_restore_replay_equal":True,
            "runtime_checkpoint_equal":True,"nonempty_restore_refused":refused,
            "source_deleted":restored["source_deleted"],
            "state_sha256":hashlib.sha256(encoded(source["state"])).hexdigest(),
            "full_commands":source["loaded_commands"],"runtime_commands":runtime["loaded_commands"]}
    if args.clickhouse:
        client=ClickHouse()
        import_archive(args.archive,client)
        # Deliberately repeat the full ingestion. FINAL results must be exact.
        import_archive(args.archive,client)
        report["clickhouse_retry_verified"]=verify_mirror(args.archive,client)["clickhouse_verified"]
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_bytes(encoded(report))
    print(json.dumps(report,ensure_ascii=False))


if __name__=="__main__":
    main()

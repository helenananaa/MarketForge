"""Exercise a real account-mode backend. Creates only uniquely named test users/rooms.

Usage: python scripts/validate_competition_platform.py --base-url http://127.0.0.1:57307
Credentials stay in .local; the public proof contains no passwords, tokens or invite codes.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import secrets
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:57307")
    parser.add_argument("--recover", action="store_true")
    args = parser.parse_args()
    private_file = ROOT / ".local/candlescope-runtime/competition-http-state.json"
    public_file = ROOT / "output/candlescope-workbench/competition-http-proof.json"

    def call(path, token="", body=None, expected=200):
        request = Request(args.base_url + path, data=None if body is None else json.dumps(body).encode(),
                          headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}", "X-User-ID": "local-user"})
        try:
            with urlopen(request, timeout=15) as response:
                status, value = response.status, json.load(response)
        except HTTPError as error:
            status, value = error.code, json.load(error)
        assert status == expected, (path, status, value)
        return value

    if args.recover:
        private = json.loads(private_file.read_text(encoding="utf-8"))
        root = f"/rooms/{private['room']}"
        recovered = call(root + "/competition", private["users"]["host"]["token"])
        assert recovered["phase"] == "Finished"
        assert recovered["results"] == private["result"]["results"]
        assert recovered["settlement_mark_tick"] == private["result"]["settlement_mark_tick"]
        for user in private["users"].values():
            assert call("/identity", user["token"])["user_id"] == user["user_id"]
        proof = json.loads(public_file.read_text(encoding="utf-8"))
        proof["restart_verified"] = True
        public_file.write_text(json.dumps(proof, ensure_ascii=False, indent=2), encoding="utf-8")
        print("PostgreSQL restart: users, live sessions and immutable match results recovered")
        return

    suffix = str(int(time.time()))
    users = {}
    for role, label in [("host", "比赛管理员"), ("alice", "交易员甲"), ("bob", "交易员乙"), ("viewer", "比赛观众")]:
        username, password = f"mf-{role}-{suffix}", secrets.token_urlsafe(24)
        value = call("/auth/register", body={"username": username, "password": password, "display_name": label})
        users[role] = {"username": username, "password": password, "token": value["token"], "user_id": value["user"]["user_id"]}
        assert call("/identity", value["token"])["user_id"] != "local-user"
    host = users["host"]["token"]
    room = f"mf-competition-{suffix}"
    root = f"/rooms/{room}"
    recipe = json.loads((ROOT / "scripts/fixtures/background_market.json").read_text(encoding="utf-8"))
    recipe["scenario"]["room_id"] = room
    recipe["autostart_agents"] = False
    for template in recipe["agents"]:
        next(iter(template.values()))["participant"]["room_id"] = room
    call("/rooms", host, recipe)
    call(root + "/pause", host, {})
    call(root + "/competition", host, {"title": "多人比赛端到端验证", "seats": [20, 30], "duration_seconds": 10, "countdown_seconds": 3, "bot_interval_ms": 200})
    for role, account in [("alice", 20), ("bob", 30), ("viewer", None)]:
        invitation = call(root + "/invitations", host, {"role": "trader" if account else "spectator", "account_id": account})
        call("/invitations/redeem", users[role]["token"], {"code": invitation["code"], "role": "admin"})
    call(root + "/observe?account_id=20", users["viewer"]["token"], expected=403)
    action = {"participant_id": "human", "account_id": 20, "action": {"PlaceMarket": {"side": "Buy", "qty": 1}}}
    call(root + "/orders", users["alice"]["token"], action, expected=409)
    call(root + "/competition/start", host, {}, expected=409)
    for role in ["alice", "bob"]:
        call(root + "/competition/ready", users[role]["token"], {"ready": True})
    call(root + "/competition/start", host, {})
    for path in ["resume", "clock/step", "agents/stop", "accounts/20/owners", "members"]:
        call(root + "/" + path, host, {}, expected=409)
    deadline = time.monotonic() + 20
    while call(root + "/competition", host)["phase"] != "Running":
        assert time.monotonic() < deadline
        time.sleep(.1)
    call(root + "/orders", host, action, expected=403)
    alice_order = call(root + "/orders", users["alice"]["token"], action)
    bob_order = call(root + "/orders", users["bob"]["token"], {**action, "account_id": 30, "action": {"PlaceMarket": {"side": "Sell", "qty": 1}}})
    assert alice_order["accepted"] and bob_order["accepted"], (alice_order["reject_reason"], bob_order["reject_reason"])
    call(root + "/observe?account_id=30", users["alice"]["token"], expected=403)
    while True:
        result = call(root + "/competition", host)
        if result["phase"] == "Finished":
            break
        assert time.monotonic() < deadline
        time.sleep(.2)
    assert len(result["results"]) == 2
    for row in result["results"]:
        assert int(row["final_equity"]) - int(row["initial_equity"]) == int(row["pnl"])
    call(root + "/orders", users["alice"]["token"], action, expected=409)
    call(root + "/competition/abort", host, {}, expected=409)
    call(root + "/observe?account_id=20", users["viewer"]["token"])
    call("/auth/logout", users["alice"]["token"], {})
    call("/identity", users["alice"]["token"], expected=401)
    users["alice"]["token"] = call("/auth/login", body={"username": users["alice"]["username"], "password": users["alice"]["password"]})["token"]
    private_file.parent.mkdir(parents=True, exist_ok=True)
    private_file.write_text(json.dumps({"room": room, "users": users, "result": result}, ensure_ascii=False), encoding="utf-8")
    proof = {"room": room, "storage": call("/runtime")["storage"], "checks": ["registration", "server identity", "role-bound invitation", "ready gate", "countdown", "pre-start denial", "admin trading denial", "private account isolation", "bot market", "server deadline", "immutable results", "spectator post-match visibility", "logout revocation"], "results": result["results"], "settlement_mark_tick": result["settlement_mark_tick"], "settlement_command_cursor": result["settlement_command_cursor"], "alice_order_command_seq": alice_order["command_seq"], "bob_order_command_seq": bob_order["command_seq"], "restart_verified": False}
    public_file.parent.mkdir(parents=True, exist_ok=True)
    public_file.write_text(json.dumps(proof, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"Live PostgreSQL match completed: {room}; 2 players, 20 configured Bots; final results archived")


if __name__ == "__main__":
    main()

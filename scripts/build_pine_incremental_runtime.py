"""Build the isolated, pinned Pine external-session adapter without changing the upstream checkout."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
COMMIT = "8838ed004c7475ce2877189bcd0e43a23bcf365f"
PATCH = ROOT / "runtime-patches/pine-compat-0.3.1-external-session.patch"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-repo", type=Path, required=True)
    parser.add_argument("--install", action="store_true", help="install only into the Pine plugin's private .runtime directory")
    parser.add_argument("--strategy-extensions", action="store_true", help="add qualified external order notification arguments in a separate private runtime")
    args = parser.parse_args()
    workspace = ROOT / ".local/pine-incremental-build"
    workspace.mkdir(parents=True, exist_ok=True)
    source = Path(tempfile.mkdtemp(prefix="source-", dir=workspace))
    archive = source.with_suffix(".zip")
    subprocess.run(["git", "-C", str(args.source_repo), "archive", COMMIT, "--output=" + str(archive)], check=True)
    with zipfile.ZipFile(archive) as bundle:
        for member in bundle.infolist():
            if not (source / member.filename).resolve().is_relative_to(source.resolve()):
                raise ValueError("archive path escapes isolated build")
        bundle.extractall(source)
    subprocess.run(["git", "apply", "--whitespace=error", str(PATCH)], cwd=source, check=True)
    extension_patch = ROOT / "runtime-patches/pine-compat-0.3.1-order-notifications.patch"
    if args.strategy_extensions:
        subprocess.run(["git", "apply", "--whitespace=error", str(extension_patch)], cwd=source, check=True)
    # Session appends may omit plot output; the broker reads only intents.
    session_patch = ROOT / "runtime-patches/pine-compat-0.3.1-session-output.patch"
    subprocess.run(["git", "apply", "--whitespace=error", str(session_patch)], cwd=source, check=True)
    env = os.environ.copy()
    env.update(CARGO_TARGET_DIR=str(workspace / "target"), PYO3_PYTHON=sys.executable)
    subprocess.run(["cargo", "build", "--locked", "--release", "-p", "pine-python"], cwd=source, env=env, check=True)
    artifact = workspace / "target/release" / ("pine_compat.dll" if os.name == "nt" else "libpine_compat.so")
    if sys.platform == "darwin": artifact = artifact.with_name("libpine_compat.dylib")
    receipt = {"version": "0.3.1+mf1", "source_commit": COMMIT,
               "patch_sha256": hashlib.sha256(PATCH.read_bytes()).hexdigest(),
               "session_patch_sha256": hashlib.sha256(session_patch.read_bytes()).hexdigest(),
               "binary_sha256": hashlib.sha256(artifact.read_bytes()).hexdigest(),
               "source": str(source), "artifact": str(artifact),
               "scope": "private adapter build; not an upstream published release",
               "capabilities": ["session_output_optional"]}
    if args.strategy_extensions:
        receipt["extension_patch_sha256"] = hashlib.sha256(extension_patch.read_bytes()).hexdigest()
        receipt["capabilities"] = ["external_comment", "external_alert_message", "external_disable_alert",
                                   "session_output_optional"]
    if args.install:
        destination = ROOT / "bot-plugins/pine-strategy/.runtime"
        if args.strategy_extensions:
            destination = destination / "strategy-ecosystem"
        destination.mkdir(parents=True, exist_ok=True)
        shutil.copy2(artifact, destination / ("pine_compat.pyd" if os.name == "nt" else "pine_compat.so"))
        shutil.copy2(source / "LICENSE", destination / "LICENSE")
        (destination / "provenance.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    (workspace / "build.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__": main()

"""Immutable strategy projects; no model-supplied path reaches the host filesystem."""
import hashlib
import json
import re
from pathlib import PurePosixPath


def normalize_project(args):
    if "code" in args and "files" in args:
        raise ValueError("provide code or files, not both")
    files = args.get("files", {"strategy.py": args.get("code", "")})
    if not isinstance(files, dict) or not 1 <= len(files) <= 64:
        raise ValueError("project requires 1-64 text files")
    for path, content in files.items():
        parsed = PurePosixPath(path)
        if (not isinstance(path, str) or len(path) > 200 or parsed.is_absolute()
                or any(part in ("..", ".") for part in path.split("/"))
                or not re.fullmatch(r"[A-Za-z0-9_./-]+", path) or str(parsed) != path):
            raise ValueError("project files must use relative POSIX paths without traversal")
        if not isinstance(content, str) or "\x00" in content:
            raise ValueError("project files must contain text")
    if sum(len(v.encode()) for v in files.values()) > 1_048_576:
        raise ValueError("project exceeds 1 MiB")
    # Reject file/directory collisions before creating anything in a container.
    for path in files:
        if any(str(parent) in files for parent in PurePosixPath(path).parents if str(parent) != "."):
            raise ValueError("project contains a file/directory collision")
    entry = args.get("entrypoint", "strategy.py")
    if entry not in files or not entry.endswith(".py"):
        raise ValueError("entrypoint must name a Python file in the project")
    requirements = args.get("requirements", [])
    if not isinstance(requirements, list) or len(requirements) > 64:
        raise ValueError("requirements must be a list of at most 64 pip requirements")
    for requirement in requirements:
        package = r"[A-Za-z0-9][A-Za-z0-9_.-]*(?:\[[A-Za-z0-9_,.-]+\])?"
        if (not isinstance(requirement, str) or len(requirement) > 512 or any(ord(c) < 32 for c in requirement)
                or not re.fullmatch(package + r"(?:\s*(?:[<>=!~].*|;.*|@\s*(?:https://|git\+https://)\S+))?", requirement)):
            raise ValueError("use a package requirement such as pandas==2.2.3, an extra, or name @ https://...; pip options/local paths are not accepted")
    packages = args.get("system_packages", [])
    if not isinstance(packages, list) or len(packages) > 32 or any(not isinstance(p, str) or not re.fullmatch(r"[a-z0-9][a-z0-9+.-]{0,99}", p) for p in packages):
        raise ValueError("system_packages must be Debian package names")
    return {"files": dict(sorted(files.items())), "entrypoint": entry,
            "requirements": list(requirements), "system_packages": list(packages)}


def project_version(project):
    return hashlib.sha256(json.dumps(project, sort_keys=True, ensure_ascii=False).encode()).hexdigest()

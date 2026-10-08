"""Per-project dependency images and disposable code runners; never execute on host."""
import json
import os
import subprocess
import threading
import time
import uuid

from .projects import normalize_project

INSTALLER = r'''
import json, os, signal, subprocess, sys, tempfile
signal.signal(signal.SIGALRM, lambda *_: os._exit(124))
signal.alarm(300)
request = json.load(sys.stdin)
def run(args):
    subprocess.run(args, check=True, stdout=sys.stderr, stderr=sys.stderr)
os.environ["DEBIAN_FRONTEND"] = "noninteractive"
os.environ["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
if request["system_packages"]:
    run(["apt-get", "-o", "APT::Sandbox::User=root", "update"])
    run(["apt-get", "-o", "APT::Sandbox::User=root", "install", "-y", "--no-install-recommends", *request["system_packages"]])
if request["requirements"]:
    with tempfile.NamedTemporaryFile(mode="w", suffix=".txt") as req:
        req.write("\n".join(request["requirements"]))
        req.flush()
        run([sys.executable, "-m", "pip", "install", "--no-cache-dir", "--report", "/tmp/install-report.json", "-r", req.name])
freeze = subprocess.check_output([sys.executable, "-m", "pip", "freeze", "--all"], text=True).splitlines()
system = subprocess.check_output(["dpkg-query", "-W", "-f=${Package}=${Version}\n"], text=True).splitlines()
report = {}
if os.path.exists("/tmp/install-report.json"):
    report = json.load(open("/tmp/install-report.json"))
print(json.dumps({"packages": freeze, "system_packages": system, "pip_report": report}))
'''

RUNNER = r'''
import contextlib, json, os, pathlib, runpy, sys
request = json.load(sys.stdin)
root = pathlib.Path("/work/project")
root.mkdir(parents=True, exist_ok=True)
for relative, content in request["project"]["files"].items():
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
os.chdir(root)
sys.path.insert(0, str(root))
observations = request["observations"]
if request.get("analysis") is not None:
    scope = {"__name__": "analysis", "observations": observations, "state": request.get("state", {})}
    with contextlib.redirect_stdout(sys.stderr):
        exec(compile(request["analysis"], "analysis.py", "exec"), scope)
    result = {"result": scope.get("result"), "orders_submitted": False}
else:
    with contextlib.redirect_stdout(sys.stderr):
        scope = runpy.run_path(str(root / request["project"]["entrypoint"]), run_name="strategy")
        result = scope["decide"](observations, request.get("state", {}))
print(json.dumps(result, allow_nan=False))
'''


class DockerSandbox:
    def __init__(self, image="marketforge-strategy:1"):
        self.image = image
        distro = os.environ.get("MARKETFORGE_AGENT_DOCKER_WSL")
        self.docker = ["wsl", "-d", distro, "--exec", "docker"] if os.name == "nt" and distro else ["docker"]
        self.builders = threading.BoundedSemaphore(2)

    def check(self):
        try:
            result = subprocess.run([*self.docker, "image", "inspect", self.image], capture_output=True, timeout=5)
            return {"available": result.returncode == 0, "image": self.image,
                    "detail": "ready" if result.returncode == 0 else "Start Docker and build the strategy image; host execution is disabled."}
        except (OSError, subprocess.TimeoutExpired):
            return {"available": False, "image": self.image, "detail": "Docker unavailable; host execution is disabled."}

    def _command(self, *args, timeout=20):
        result = subprocess.run([*self.docker, *args], capture_output=True, timeout=timeout)
        if result.returncode:
            raise ValueError("container operation failed: " + result.stderr.decode(errors="replace")[-2000:])
        return result.stdout.decode().strip()

    def _remove(self, name):
        try:
            subprocess.run([*self.docker, "rm", "-f", name], capture_output=True, timeout=5)
        except (OSError, subprocess.TimeoutExpired):
            pass

    def _capture(self, command, payload, timeout, cancel=None, progress=None):
        proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        chunks = {"out": bytearray(), "err": bytearray()}
        overflow = threading.Event()
        def drain(pipe, key):
            while data := pipe.read(4096):
                if key == "err" and progress:
                    progress(data.decode(errors="replace"))
                if len(chunks[key]) + len(data) > 1_048_576:
                    if key == "err":
                        del chunks[key][:len(data)]
                    else:
                        overflow.set(); proc.kill(); break
                chunks[key].extend(data)
        readers = [threading.Thread(target=drain, args=(proc.stdout, "out"), daemon=True),
                   threading.Thread(target=drain, args=(proc.stderr, "err"), daemon=True)]
        for thread in readers:
            thread.start()
        def write():
            try:
                proc.stdin.write(payload); proc.stdin.close()
            except (BrokenPipeError, OSError):
                pass
        writer = threading.Thread(target=write, daemon=True); writer.start()
        deadline = time.monotonic() + timeout
        try:
            while proc.poll() is None:
                if cancel is not None and cancel.is_set():
                    raise ValueError("environment installation cancelled")
                if time.monotonic() >= deadline:
                    raise ValueError("container exceeded execution deadline")
                time.sleep(0.05)
            for thread in readers:
                thread.join(timeout=2)
            if overflow.is_set():
                raise ValueError("container output exceeds 1 MiB")
            if proc.returncode:
                raise ValueError("container failed: " + chunks["err"].decode(errors="replace")[-4000:])
            return bytes(chunks["out"]), chunks["err"].decode(errors="replace")[-12000:]
        finally:
            if proc.poll() is None:
                proc.kill(); proc.wait(timeout=3)
            writer.join(timeout=2)
            for thread in readers:
                thread.join(timeout=2)
            for pipe in (proc.stdin, proc.stdout, proc.stderr):
                if not pipe.closed:
                    pipe.close()

    def prepare(self, project, cancel=None, progress=None):
        while not self.builders.acquire(timeout=0.2):
            if cancel is not None and cancel.is_set():
                raise ValueError("environment installation cancelled")
        try:
            return self._prepare(project, cancel, progress)
        finally:
            self.builders.release()

    def _prepare(self, project, cancel, progress):
        project = normalize_project(project)
        name = "mf-install-" + uuid.uuid4().hex
        if cancel is not None and cancel.is_set():
            raise ValueError("environment installation cancelled")
        base_id = self._command("image", "inspect", "--format", "{{.Id}}", self.image)
        if not project["requirements"] and not project["system_packages"]:
            return {"image": base_id, "base_image": base_id, "packages": [], "system_packages": [], "log": "No extra dependencies requested."}
        try:
            self._command("create", "-i", "--name", name, "--network=bridge", "--user=0",
                "--cap-drop=ALL", "--cap-add=CHOWN", "--cap-add=DAC_OVERRIDE", "--cap-add=FOWNER",
                "--cap-add=SETUID", "--cap-add=SETGID", "--security-opt=no-new-privileges",
                "--pids-limit=128", "--memory=1g", "--memory-swap=1g", "--cpus=2", "--log-driver=none",
                base_id, "python", "-I", "-c", INSTALLER)
            raw, log = self._capture([*self.docker, "start", "-ai", name],
                json.dumps({"requirements": project["requirements"], "system_packages": project["system_packages"]}).encode(),
                timeout=300, cancel=cancel, progress=progress)
            receipt = json.loads(raw)
            if cancel is not None and cancel.is_set():
                raise ValueError("environment installation cancelled")
            tag = "marketforge-agent-env:" + uuid.uuid4().hex
            image_id = self._command("commit", "--change", "USER 65534:65534", name, tag, timeout=30)
            return {"image": image_id, "tag": tag, "base_image": base_id, **receipt, "log": log}
        finally:
            self._remove(name)

    def run(self, code, observations, state):
        return self.run_project(normalize_project({"code": code}), None, observations, state)

    def run_project(self, project, environment, observations, state, analysis=None):
        project = normalize_project(project)
        if (project["requirements"] or project["system_packages"]) and not environment:
            raise ValueError("install this project's dependencies before testing or running")
        image = environment["image"] if environment else self.image
        name = "mf-strategy-" + uuid.uuid4().hex
        command = [*self.docker, "run", "--rm", "--pull=never", "--name", name, "--network=none",
                   "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--user=65534:65534",
                   "--tmpfs", "/work:rw,noexec,nosuid,size=32m,uid=65534,gid=65534",
                   "--pids-limit=64", "--memory=512m", "--memory-swap=512m", "--cpus=1",
                   "--ulimit", "cpu=10:10", "--ulimit", "nofile=128:128", "--log-driver=none",
                   "-e", "OPENBLAS_NUM_THREADS=1", "-e", "OMP_NUM_THREADS=1",
                   "-e", "HOME=/work", "-e", "TMPDIR=/work", "-e", "XDG_CACHE_HOME=/work/.cache",
                   "-i", image, "python", "-I", "-B", "-c", RUNNER]
        payload = json.dumps({"project": project, "observations": observations, "state": state, "analysis": analysis}, allow_nan=False).encode()
        if len(payload) > 3_000_000:
            raise ValueError("strategy input exceeds 3 MB")
        try:
            raw, log = self._capture(command, payload, timeout=20)
            result = json.loads(raw)
            if analysis is not None:
                return {**result, "output": log, "orders_submitted": False}
            if not isinstance(result, dict) or set(result) - {"actions", "state", "summary"}:
                raise ValueError("return {actions: [...], state: {...}, summary: '...'}")
            if not isinstance(result.get("actions", []), list) or len(result.get("actions", [])) > 8:
                raise ValueError("at most 8 actions per strategy tick")
            if len(json.dumps(result.get("state", {}))) > 65536:
                raise ValueError("strategy state exceeds 64 KiB")
            return result
        finally:
            self._remove(name)

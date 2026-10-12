"""Sample Windows server + descendants without an optional psutil dependency."""
import ctypes
from ctypes import wintypes
import os
import threading
import time


class ProcessTreeMetrics:
    def __init__(self, root):
        self.root = root
        self.known = {root}
        self.samples = []
        self.errors = []
        self.compiler_activity = []
        self.other_servers = []
        self.stop = threading.Event()
        self.begun = time.perf_counter()
        self.measurement_window = None
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _sample(self):
        if os.name != "nt":
            raise RuntimeError("process-tree resource sampling currently requires Windows")
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        class Entry(ctypes.Structure):
            _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD),
                        ("pid", wintypes.DWORD), ("heap", ctypes.c_size_t),
                        ("module", wintypes.DWORD), ("threads", wintypes.DWORD),
                        ("parent", wintypes.DWORD), ("priority", wintypes.LONG),
                        ("flags", wintypes.DWORD), ("exe", wintypes.WCHAR * 260)]
        class Memory(ctypes.Structure):
            _fields_ = [("size", wintypes.DWORD), ("faults", wintypes.DWORD)] + [
                (name, ctypes.c_size_t) for name in ("peak_rss", "rss", "peak_paged", "paged",
                "peak_nonpaged", "nonpaged", "pagefile", "peak_pagefile")]
        kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
        kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
        kernel.Process32FirstW.argtypes = kernel.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
        psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Memory), wintypes.DWORD]
        snapshot = kernel.CreateToolhelp32Snapshot(2, 0)
        if snapshot == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        parents = {}
        compilers, servers = {}, {}
        try:
            entry = Entry();entry.size = ctypes.sizeof(entry)
            ok = kernel.Process32FirstW(snapshot, ctypes.byref(entry))
            while ok:
                parents[entry.pid] = entry.parent
                name = entry.exe.casefold()
                if name in {"cargo.exe", "rustc.exe", "cl.exe", "link.exe"}:
                    compilers[entry.pid] = entry.exe
                if name in {"exchange-server.exe", "diagnostic-server.exe", "candidate-server.exe", "before-server.exe"} and entry.pid != self.root:
                    servers[entry.pid] = entry.exe
                ok = kernel.Process32NextW(snapshot, ctypes.byref(entry))
        finally:
            kernel.CloseHandle(snapshot)
        self.compiler_activity.append(compilers)
        self.other_servers.append(servers)
        current = {self.root} if self.root in parents else set()
        while True:
            next_set = current | {pid for pid, parent in parents.items() if parent in current}
            if next_set == current: break
            current = next_set
        self.known.update(current)
        values = {}
        for pid in current:
            handle = kernel.OpenProcess(0x400 | 0x10, False, pid)
            if not handle: continue  # Process may have exited since the snapshot.
            try:
                times = [wintypes.FILETIME() for _ in range(4)]
                memory = Memory();memory.size = ctypes.sizeof(memory)
                if not kernel.GetProcessTimes(handle, *[ctypes.byref(t) for t in times]):
                    raise ctypes.WinError(ctypes.get_last_error())
                if not psapi.GetProcessMemoryInfo(handle, ctypes.byref(memory), memory.size):
                    raise ctypes.WinError(ctypes.get_last_error())
                values[pid] = {"cpu_seconds":sum((t.dwHighDateTime << 32) + t.dwLowDateTime for t in times[2:]) / 10_000_000,
                               "rss_bytes":memory.rss}
            finally:
                kernel.CloseHandle(handle)
        return values

    def _run(self):
        previous = {};cpu = 0.0
        begun = self.begun
        while not self.stop.is_set():
            try:
                values = self._sample()
                for pid, value in values.items():
                    cpu += max(0, value["cpu_seconds"] - previous.get(pid, 0))
                    previous[pid] = value["cpu_seconds"]
                self.samples.append({"elapsed":time.perf_counter()-begun, "cpu_seconds":cpu,
                    "rss_bytes":sum(v["rss_bytes"] for v in values.values()), "processes":len(values),
                    "per_process":values})
            except Exception as error:
                self.errors.append(str(error));break
            self.stop.wait(.25)

    def finish(self):
        self.stop.set();self.thread.join(timeout=2)
        measured = [s for s in self.samples if self.measurement_window is not None
                    and self.measurement_window[0] <= s["elapsed"] <= self.measurement_window[1]]
        window = {"scope":"samples wholly inside the requested measurement window; excludes prewarm/startup/warmup",
                  "samples":len(measured),
                  "average_logical_cores":((measured[-1]["cpu_seconds"]-measured[0]["cpu_seconds"]) /
                    (measured[-1]["elapsed"]-measured[0]["elapsed"])) if len(measured) > 1 else None,
                  "peak_sampled_rss_mib":max((s["rss_bytes"] for s in measured), default=0)/1024**2 if measured else None}
        individual = {}
        for sample in self.samples:
            for pid, value in sample["per_process"].items():
                record = individual.setdefault(pid, {"first_elapsed":sample["elapsed"],
                    "first_cpu_seconds":value["cpu_seconds"], "peak_rss_bytes":0})
                record.update(last_elapsed=sample["elapsed"], last_cpu_seconds=value["cpu_seconds"],
                    peak_rss_bytes=max(record["peak_rss_bytes"],value["rss_bytes"]))
        for record in individual.values():
            elapsed = record["last_elapsed"] - record["first_elapsed"]
            record["average_logical_cores_while_sampled"] = (
                (record["last_cpu_seconds"] - record["first_cpu_seconds"]) / elapsed
                if elapsed > 0 else None)
        return {"scope":"server and live descendants, sampled every 250ms including startup/warmup/measurement",
                "limitations":"sampled summed working set; shared pages counted per process; short-lived processes may be missed",
                "peak_sampled_rss_mib":max((s["rss_bytes"] for s in self.samples), default=0)/1024**2 if self.samples else None,
                "peak_processes":max((s["processes"] for s in self.samples), default=None),
                "average_logical_cores":(self.samples[-1]["cpu_seconds"] / self.samples[-1]["elapsed"]) if self.samples else None,
                "seen_pids":sorted(self.known), "per_process":individual,
                "compiler_nonempty_samples":sum(bool(v) for v in self.compiler_activity),
                "other_server_nonempty_samples":sum(bool(v) for v in self.other_servers),
                "measurement":window, "errors":self.errors, "samples":self.samples}

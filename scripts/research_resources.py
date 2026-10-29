"""Small, direct resource samples; never equate commitment with physical usage."""
import ctypes
from ctypes import wintypes
import os
import json
from pathlib import Path
import subprocess


def windows_memory(pid=None):
    class Performance(ctypes.Structure):
        _fields_ = [('cb', wintypes.DWORD)] + [(name, ctypes.c_size_t) for name in (
            'CommitTotal', 'CommitLimit', 'CommitPeak', 'PhysicalTotal', 'PhysicalAvailable',
            'SystemCache', 'KernelTotal', 'KernelPaged', 'KernelNonpaged', 'PageSize')
        ] + [(name, wintypes.DWORD) for name in ('HandleCount', 'ProcessCount', 'ThreadCount')]
    psapi = ctypes.WinDLL('psapi', use_last_error=True)
    info = Performance(); info.cb = ctypes.sizeof(info)
    psapi.GetPerformanceInfo.argtypes = [ctypes.POINTER(Performance), wintypes.DWORD]
    psapi.GetPerformanceInfo.restype = wintypes.BOOL
    if not psapi.GetPerformanceInfo(ctypes.byref(info), info.cb):
        raise ctypes.WinError(ctypes.get_last_error())
    result = dict(host_commit_bytes=info.CommitTotal*info.PageSize,
                  host_commit_limit_bytes=info.CommitLimit*info.PageSize,
                  host_available_physical_bytes=info.PhysicalAvailable*info.PageSize,
                  free_commit_kib=(info.CommitLimit-info.CommitTotal)*info.PageSize//1024)
    if pid is not None:
        class ProcessMemory(ctypes.Structure):
            _fields_ = [('cb', wintypes.DWORD), ('PageFaultCount', wintypes.DWORD)] + [
                (name, ctypes.c_size_t) for name in ('PeakWorkingSetSize', 'WorkingSetSize',
                'QuotaPeakPagedPoolUsage', 'QuotaPagedPoolUsage', 'QuotaPeakNonPagedPoolUsage',
                'QuotaNonPagedPoolUsage', 'PagefileUsage', 'PeakPagefileUsage', 'PrivateUsage')]
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x1000, False, pid)
        if handle:
            try:
                memory = ProcessMemory(); memory.cb = ctypes.sizeof(memory)
                psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(ProcessMemory), wintypes.DWORD]
                psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
                if psapi.GetProcessMemoryInfo(handle, ctypes.byref(memory), memory.cb):
                    result.update(process_private_bytes=memory.PrivateUsage,
                                  process_working_set_bytes=memory.WorkingSetSize,
                                  process_peak_private_bytes=memory.PeakPagefileUsage)
            finally:
                kernel.CloseHandle(handle)
    return result


def stop_reason(sample, elapsed, private_limit_gib=6, timeout=10800):
    if sample['free_commit_kib'] < 3*1024*1024:
        return 'host_commit_headroom_below_3GiB'
    if sample.get('process_private_bytes', 0) > private_limit_gib*1024**3:
        return 'owned_process_private_limit_exceeded'
    if elapsed > timeout:
        return 'wall_timeout'
    return None


def reclaim_guest_file_cache(request_bytes=1024**3):
    if not 0 < request_bytes <= 1024**3:
        raise ValueError('File-cache reclaim requests must be within 1 GiB')
    try:
        return _reclaim_guest_file_cache(request_bytes)
    except (OSError,subprocess.SubprocessError,ValueError,KeyError)as error:
        return dict(reclaimed=False,error=str(error))


def _reclaim_guest_file_cache(request_bytes):
    """One <=1 GiB file-only reclaim, with spare guest RAM/cache prerequisites.

    Called only by a launch gate with a per-batch budget. Does not alter limits,
    run drop_caches, restart WSL, stop processes, or swap anonymous memory.
    """
    if not 0 < request_bytes <= 1024**3:
        raise ValueError('File-cache reclaim requests must be within 1 GiB')
    if os.name=='nt':
        running=subprocess.run(['wsl','--list','--running','--quiet'],capture_output=True,timeout=10)
        if running.returncode or not running.stdout.replace(b'\0',b'').strip():
            return dict(reclaimed=False,error='No existing running WSL guest')
        script=("import json;from pathlib import Path;"
                "d={s.split(':')[0]:int(s.split()[1])*1024 for s in Path('/proc/meminfo').read_text().splitlines()};"
                f"n={request_bytes};"
                "assert d['MemAvailable']>=n+4*1024**3 and d['Cached']-d['Shmem']>=n+1024**3,'insufficient spare guest cache';"
                "Path('/sys/fs/cgroup/memory.reclaim').write_text(str(n)+' swappiness=0');"
                "print(json.dumps(dict(reclaimed=True,requested_bytes=n,before=d)))")
        completed=subprocess.run(['wsl','--user','root','--exec','python3','-c',script],capture_output=True,timeout=30)
        if completed.returncode:
            return dict(reclaimed=False,error=completed.stderr.decode(errors='replace')[-1000:])
        return json.loads(completed.stdout)
    fields={s.split(':')[0]:int(s.split()[1])*1024 for s in Path('/proc/meminfo').read_text().splitlines()}
    if fields['MemAvailable']<request_bytes+4*1024**3 or fields['Cached']-fields['Shmem']<request_bytes+1024**3:
        return dict(reclaimed=False,error='Insufficient spare guest cache')
    try:
        Path('/sys/fs/cgroup/memory.reclaim').write_text(str(request_bytes)+' swappiness=0')
        return dict(reclaimed=True,requested_bytes=request_bytes,before=fields)
    except OSError as error:
        return dict(reclaimed=False,error=str(error))

"""Read-only Windows commitment relay for this task's native guard."""
import json,time
from pathlib import Path
from research_resources import windows_memory

BASE=Path(__file__).resolve().parents[1]/'.local/strategy-research-parent-liquidity-20261025'
if __name__=='__main__':
    deadline=time.monotonic()+3*3600
    while time.monotonic()<deadline and not (BASE/'host-memory-monitor.stop').exists():
        m=windows_memory()
        pulse=dict(unix_seconds=time.time(),free_commit_kib=m['free_commit_kib'],available_physical_kib=m['host_available_physical_bytes']//1024)
        tmp=BASE/'host-memory.tmp';tmp.write_text(json.dumps(pulse)+'\n');tmp.replace(BASE/'host-memory.json')
        time.sleep(5)

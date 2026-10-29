"""Atomic pulse publication; transient Windows sharing failures remain visible."""
import json,time
from research_resources import windows_memory


def tick(base,sample=None,clock=None):
    try:
        m=(sample or windows_memory)()
        pulse=dict(unix_seconds=(clock or time.time)(),free_commit_kib=m['free_commit_kib'],
            available_physical_kib=m['host_available_physical_bytes']//1024)
        tmp=base/'host-memory.tmp';tmp.write_text(json.dumps(pulse)+'\n')
        tmp.replace(base/'host-memory.json')
        return dict(published=True,**pulse)
    except OSError as e:
        return dict(published=False,error=repr(e),unix_seconds=(clock or time.time)())

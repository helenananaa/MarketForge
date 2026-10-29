"""Bounded cross-OS wall-clock skew, separate from stale relay and memory limits."""
import math


def relay_sample(pulse,now):
    try:
        raw_age=float(now)-float(pulse['unix_seconds']);headroom=int(pulse['free_commit_kib'])
        valid=math.isfinite(raw_age)and -2<=raw_age<30 and headroom>=0
    except (TypeError,ValueError,KeyError,OverflowError):raw_age=None;headroom=0;valid=False
    if raw_age is not None and not math.isfinite(raw_age):raw_age=None
    return dict(host_free_commit_kib=headroom if valid else 0,
        host_heartbeat_age_seconds=max(0.,raw_age)if valid else raw_age if raw_age is not None else -1.,
        host_heartbeat_raw_age_seconds=raw_age,host_heartbeat_valid=valid,
        host_clock_skew_clamped=bool(valid and raw_age<0))

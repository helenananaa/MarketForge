//! Process-wide measurement for the offline benchmark, never runtime accounting.

#[derive(Default)]
pub struct ProcessMemory {
    pub peak_resident_kib: Option<u64>,
    pub peak_commit_kib: Option<u64>,
    pub resident_kib: Option<u64>,
    pub commit_kib: Option<u64>,
}

pub fn read() -> ProcessMemory {
    platform_memory()
}

pub const SOURCE: &str = if cfg!(windows) {
    "windowsPeakWorkingSet"
} else if cfg!(target_os = "linux") {
    "linuxVmHWM"
} else {
    "unsupportedPlatform"
};

#[cfg(target_os = "linux")]
fn platform_memory() -> ProcessMemory {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
    };
    ProcessMemory {
        peak_resident_kib: field("VmHWM:"),
        peak_commit_kib: None,
        resident_kib: field("VmRSS:"),
        commit_kib: None,
    }
}

#[cfg(windows)]
fn platform_memory() -> ProcessMemory {
    use std::ffi::c_void;

    // PROCESS_MEMORY_COUNTERS from psapi.h. DWORD is u32 and SIZE_T is usize.
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }

    let Ok(size) = u32::try_from(std::mem::size_of::<ProcessMemoryCounters>()) else {
        return ProcessMemory::default();
    };
    let mut counters = ProcessMemoryCounters {
        cb: size,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle identifies this live process and needs no close.
    // The writable repr(C) buffer has exactly the declared size and stays alive
    // for the synchronous call. No data is read on an API failure.
    let success = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    if success == 0 {
        return ProcessMemory::default();
    }
    ProcessMemory {
        peak_resident_kib: Some(counters.peak_working_set_size as u64 / 1024),
        peak_commit_kib: Some(counters.peak_pagefile_usage as u64 / 1024),
        resident_kib: Some(counters.working_set_size as u64 / 1024),
        commit_kib: Some(counters.pagefile_usage as u64 / 1024),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_memory() -> ProcessMemory {
    ProcessMemory::default()
}

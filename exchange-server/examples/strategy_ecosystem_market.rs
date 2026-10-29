//! Actual installed process Pine + native strategies, real matching research.
#[allow(dead_code)]
#[path = "../../exchange-core/examples/strategy_market.rs"]
mod research;
// Each order makes a few hundred small allocations, and the system heap slows
// as account count grows; mimalloc keeps them cheap without changing results.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
mod cpu_profile {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

    #[repr(C, align(16))]
    struct Context([u8; 1232]);
    impl Context {
        fn new() -> Self {
            let mut c = Context([0; 1232]);
            // CONTEXT_FULL (AMD64 | CONTROL | INTEGER | FLOATING_POINT)
            c.0[0x30..0x34].copy_from_slice(&0x0010_000Bu32.to_le_bytes());
            c
        }
        fn get(&self, at: usize) -> u64 {
            u64::from_le_bytes(self.0[at..at + 8].try_into().unwrap())
        }
        fn set(&mut self, at: usize, v: u64) {
            self.0[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
    }
    const RSP: usize = 0x98;
    const RIP: usize = 0xF8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn OpenThread(access: u32, inherit: i32, id: u32) -> *mut c_void;
        fn SuspendThread(h: *mut c_void) -> u32;
        fn ResumeThread(h: *mut c_void) -> u32;
        fn GetThreadContext(h: *mut c_void, ctx: *mut Context) -> i32;
        fn RtlLookupFunctionEntry(pc: u64, base: *mut u64, history: *mut c_void) -> *mut c_void;
        fn RtlVirtualUnwind(
            kind: u32,
            base: u64,
            pc: u64,
            entry: *mut c_void,
            ctx: *mut Context,
            handler_data: *mut *mut c_void,
            establisher: *mut u64,
            pointers: *mut c_void,
        ) -> *mut c_void;
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static STOP: AtomicBool = AtomicBool::new(false);
    static SAMPLES: Mutex<Vec<Vec<u64>>> = Mutex::new(Vec::new());

    pub fn set_active(on: bool) {
        ACTIVE.store(on, Relaxed);
    }

    // Runs while the target is suspended: no allocation, no locks.
    unsafe fn walk(ctx: &mut Context, out: &mut [u64; 160]) -> usize {
        let mut n = 0;
        while n < out.len() {
            let pc = ctx.get(RIP);
            if pc == 0 {
                break;
            }
            out[n] = pc;
            n += 1;
            let mut base = 0u64;
            let entry = unsafe { RtlLookupFunctionEntry(pc, &mut base, null_mut()) };
            if entry.is_null() {
                // Only a leaf (the interrupted frame) may lack unwind data.
                let rsp = ctx.get(RSP);
                if n != 1 || rsp == 0 {
                    break;
                }
                let ret = unsafe { *(rsp as *const u64) };
                ctx.set(RIP, ret);
                ctx.set(RSP, rsp + 8);
            } else {
                let mut data = null_mut();
                let mut frame = 0u64;
                unsafe {
                    RtlVirtualUnwind(0, base, pc, entry, ctx, &mut data, &mut frame, null_mut())
                };
            }
        }
        n
    }

    pub fn start() -> Option<std::thread::JoinHandle<()>> {
        std::env::var_os("CPU_SAMPLES_OUT")?;
        // THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION
        let handle = unsafe { OpenThread(0x0002 | 0x0008 | 0x0040, 0, GetCurrentThreadId()) } as usize;
        Some(std::thread::spawn(move || {
            let h = handle as *mut c_void;
            let mut frames = [0u64; 160];
            while !STOP.load(Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(1));
                if !ACTIVE.load(Relaxed) {
                    continue;
                }
                let mut ctx = Context::new();
                let n = unsafe {
                    if SuspendThread(h) == u32::MAX {
                        continue;
                    }
                    let n = if GetThreadContext(h, &mut ctx) != 0 {
                        walk(&mut ctx, &mut frames)
                    } else {
                        0
                    };
                    ResumeThread(h);
                    n
                };
                if n > 0 {
                    SAMPLES.lock().unwrap().push(frames[..n].to_vec());
                }
            }
        }))
    }

    pub fn finish(sampler: Option<std::thread::JoinHandle<()>>) {
        use std::io::Write;
        let Some(sampler) = sampler else { return };
        STOP.store(true, Relaxed);
        sampler.join().unwrap();
        let path = std::env::var_os("CPU_SAMPLES_OUT").unwrap();
        let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        let base = unsafe { GetModuleHandleW(std::ptr::null()) } as u64;
        let exe = std::env::current_exe().unwrap();
        writeln!(out, "# base={base:x} exe={}", exe.display()).unwrap();
        for sample in SAMPLES.lock().unwrap().iter() {
            let line: Vec<String> = sample.iter().map(|a| format!("{a:x}")).collect();
            writeln!(out, "{}", line.join(" ")).unwrap();
        }
    }
}
pub fn profile_submit(on: bool) {
    cpu_profile::set_active(on);
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sampler = cpu_profile::start();
    let registry =
        exchange_server::bot_plugins::load_bot_plugins(std::path::Path::new("bot-plugins"))?;
    let result = research::run(registry);
    cpu_profile::finish(sampler);
    // Diagnostic builds also report the plugin host's cumulative phase timers.
    #[cfg(feature = "bench-execution-timing")]
    for line in exchange_server::bench_execution_metrics().lines() {
        if line.starts_with("marketforge_bench_bot") && !line.ends_with(" 0") {
            eprintln!("{line}");
        }
    }
    result
}

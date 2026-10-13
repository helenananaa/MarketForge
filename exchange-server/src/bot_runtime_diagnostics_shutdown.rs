//! Explicit end-of-process trace acknowledgement; never called per event.
use super::{DECISION_SEQUENCE, TRACE_ERRORS, Trace, trace_sink};
use std::{
    io::{self, Write},
    path::Path,
    sync::atomic::Ordering,
};

#[derive(Default)]
struct Workers {
    count: std::sync::Mutex<usize>,
    done: std::sync::Condvar,
}
static WORKERS: std::sync::OnceLock<Workers> = std::sync::OnceLock::new();

pub(crate) struct WorkerGuard<'a>(&'a Workers);
impl Drop for WorkerGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            shutdown_failed();
        }
        let mut count = self
            .0
            .count
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *count -= 1;
        self.0.done.notify_all();
    }
}

impl Workers {
    fn register(&self) -> WorkerGuard<'_> {
        *self.count.lock().unwrap_or_else(|error| error.into_inner()) += 1;
        WorkerGuard(self)
    }

    fn wait(&self) -> io::Result<()> {
        let mut count = self
            .count
            .lock()
            .map_err(|_| io::Error::other("trace worker counter poisoned"))?;
        while *count != 0 {
            count = self
                .done
                .wait(count)
                .map_err(|_| io::Error::other("trace worker counter poisoned"))?;
        }
        Ok(())
    }
}

/// Register before spawning. Replaced workers may no longer have join handles
/// in the room directory, but their started blocking work must still finish.
pub(crate) fn register_worker() -> Option<WorkerGuard<'static>> {
    if !shutdown_requested() {
        return None;
    }
    let workers = WORKERS.get_or_init(Workers::default);
    Some(workers.register())
}

pub(crate) fn shutdown_requested() -> bool {
    std::env::var("MARKETFORGE_BOT_TRACE_SHUTDOWN").as_deref() == Ok("1")
}

pub(crate) fn shutdown_failed() {
    if shutdown_requested() {
        TRACE_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
}

fn complete(trace: &Trace, path: &Path, errors: u64, decision_sequence: u64) -> io::Result<()> {
    let mut writer = trace
        .writer
        .lock()
        .map_err(|_| io::Error::other("trace writer poisoned"))?;
    if errors != 0 {
        writer.flush()?;
        return Err(io::Error::other(format!(
            "trace has {errors} errors; no completion acknowledgement"
        )));
    }
    let marker = serde_json::json!({
        "schema_version":1,"stage":"trace_complete",
        "wall_us":u64::try_from(trace.origin.elapsed().as_micros()).unwrap_or(u64::MAX),
        "process_id":std::process::id(),"decision_sequence":decision_sequence,"trace_errors":errors,
    });
    serde_json::to_writer(&mut *writer, &marker)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_data()?;
    let bytes = writer.get_ref().metadata()?.len();
    let mut acknowledgement_path = path.as_os_str().to_owned();
    acknowledgement_path.push(".complete.json");
    let mut acknowledgement = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(acknowledgement_path)?;
    serde_json::to_writer(
        &mut acknowledgement,
        &serde_json::json!({
            "version":1,"trace_path":path,"trace_bytes":bytes,"marker":marker,
        }),
    )?;
    acknowledgement.write_all(b"\n")?;
    acknowledgement.sync_data()?;
    Ok(())
}

/// The server calls this only after joining trace-producing workers, completing
/// HTTP requests and draining detached durable writes. No producer may remain.
pub(crate) fn finish_trace() -> io::Result<()> {
    if !shutdown_requested() {
        return Ok(());
    }
    if let Some(workers) = WORKERS.get() {
        workers.wait()?;
    }
    let path = std::env::var_os("MARKETFORGE_BOT_TRACE")
        .ok_or_else(|| io::Error::other("trace shutdown requested without trace path"))?;
    let trace = trace_sink()
        .ok_or_else(|| io::Error::other("trace shutdown requested without a trace writer"))?;
    complete(
        trace,
        Path::new(&path),
        TRACE_ERRORS.load(Ordering::Relaxed),
        DECISION_SEQUENCE.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::BufWriter,
        sync::{Mutex, atomic::AtomicU64},
        time::Instant,
    };
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    fn fixture() -> (std::path::PathBuf, Trace) {
        let directory = std::env::temp_dir().join(format!(
            "marketforge-trace-close-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("trace.jsonl");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        (
            path,
            Trace {
                origin: Instant::now(),
                writer: Mutex::new(BufWriter::new(file)),
            },
        )
    }
    #[test]
    fn trace_shutdown_completion_covers_buffered_tail_and_exact_marker() {
        let (path, trace) = fixture();
        // Keep the entire event in BufWriter until final closeout.
        trace.writer.lock().unwrap().write_all(b"{\"schema_version\":1,\"wall_us\":0,\"stage\":\"decision_state_committed\",\"decision_id\":1}\n").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        complete(&trace, &path, 0, 1).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let rows: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        let ack: serde_json::Value = serde_json::from_slice(
            &std::fs::read(format!("{}.complete.json", path.display())).unwrap(),
        )
        .unwrap();
        assert_eq!(ack["marker"], rows[1]);
        assert_eq!(ack["trace_bytes"], bytes.len());
        assert_eq!(ack["marker"]["decision_sequence"], 1);
        drop(trace);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn trace_shutdown_errors_never_produce_success_acknowledgement() {
        let (path, trace) = fixture();
        assert!(complete(&trace, &path, 1, 0).is_err());
        assert!(!Path::new(&format!("{}.complete.json", path.display())).exists());
        // Failure to create the acknowledgement must propagate as well.
        std::fs::write(format!("{}.complete.json", path.display()), "existing").unwrap();
        assert!(complete(&trace, &path, 0, 0).is_err());
        assert_eq!(
            std::fs::read_to_string(format!("{}.complete.json", path.display())).unwrap(),
            "existing"
        );
        drop(trace);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn trace_shutdown_registry_waits_for_replaced_worker_and_started_blocking_job() {
        let workers = Workers::default();
        let (path, trace) = fixture();
        let acknowledgement = std::path::PathBuf::from(format!("{}.complete.json", path.display()));
        std::thread::scope(|scope| {
            // Registration precedes spawn, just as in AgentWorkerHandle.
            let old = workers.register();
            let (job_send, job_wait) = std::sync::mpsc::channel();
            let (started_send, started_wait) = std::sync::mpsc::channel();
            let old_handle = scope.spawn(move || {
                let _guard = old;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    tokio::task::spawn_blocking(move || {
                        started_send.send(()).unwrap();
                        job_wait.recv().unwrap();
                    })
                    .await
                    .unwrap();
                });
            });
            started_wait.recv().unwrap();
            // Replacing a room worker can detach the old handle. The new one
            // can already exit while the old blocking call is still running.
            drop(old_handle);
            let replacement = workers.register();
            scope.spawn(move || drop(replacement)).join().unwrap();
            assert_eq!(*workers.count.lock().unwrap(), 1);
            let (waiting_send, waiting_wait) = std::sync::mpsc::channel();
            let (workers, trace, path) = (&workers, &trace, &path);
            let closer = scope.spawn(move || {
                waiting_send.send(()).unwrap();
                workers.wait().unwrap();
                complete(trace, path, 0, 0).unwrap();
            });
            waiting_wait.recv().unwrap();
            assert!(!closer.is_finished());
            assert!(!acknowledgement.exists());
            job_send.send(()).unwrap();
            closer.join().unwrap();
            assert_eq!(*workers.count.lock().unwrap(), 0);
            assert!(acknowledgement.exists());
        });
        drop(trace);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

//! Lossless output spooling; never changes market inputs or execution order.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
type Error = Box<dyn std::error::Error>;
const FILE_KEY: &str = "__research_array_file";
static FILES: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
static NEXT: AtomicUsize = AtomicUsize::new(0);
static ARCHIVES: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();

pub fn spooling() -> bool {
    // Existing checkpoint merging consumes real arrays in short chunks.
    std::env::var_os("MF_RESEARCH_CHECKPOINT").is_none()
        && std::env::var("MF_RESEARCH_OUTPUT").as_deref() != Ok("memory")
}

pub struct ExecutionArchive {
    path: PathBuf,
    writer: HashWriter<BufWriter<File>>,
    records: usize,
}
impl ExecutionArchive {
    pub fn open() -> Result<Option<Self>, Error> {
        if !spooling() || std::env::var("MF_RESEARCH_COMPACTION").as_deref() == Ok("off") {
            return Ok(None);
        }
        let root = std::env::var_os("MF_RESEARCH_SPOOL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".local/research-spool"));
        std::fs::create_dir_all(&root)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = root.join(format!("executions-{}-{nonce}.jsonl", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Some(Self {
            path,
            writer: HashWriter {
                inner: BufWriter::with_capacity(128 * 1024, file),
                hash: Sha256::new(),
            },
            records: 0,
        }))
    }
    pub fn record(&mut self, execution: &exchange_core::ActorExecution) -> Result<(), Error> {
        let value=super::checkpoint::export_execution(execution).unwrap_or_else(||json!({
            "kind":"actor_rejected","room_id":execution.room_id,"instrument_id":execution.instrument_id,
            "command_seq":execution.command_seq,"market_time_ms":execution.market_time_ms,
            "command":execution.rejected_command,"reason":format!("{:?}",execution.result)}));
        serde_json::to_writer(&mut self.writer, &value)?;
        self.writer.write_all(b"\n")?;
        self.records += 1;
        Ok(())
    }
    pub fn flush(&mut self) -> Result<(), Error> {
        self.writer.flush()?;
        self.writer.inner.get_ref().sync_data()?;
        advise_cold(self.writer.inner.get_ref());
        Ok(())
    }
    pub fn finish(mut self) -> Result<(), Error> {
        self.flush()?;
        let metadata = json!({"path":self.path,"records":self.records,"sha256":format!("{:x}",self.writer.hash.finalize())});
        eprintln!("execution_archive={}", metadata);
        ARCHIVES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap()
            .push(metadata);
        Ok(())
    }
}
pub struct Array {
    memory: Vec<Value>,
    disk: Option<(PathBuf, BufWriter<File>)>,
    count: u64,
    bytes_since_advice: usize,
}
impl Array {
    pub fn new() -> Result<Self, Error> {
        let disk = if spooling() {
            let root = std::env::var_os("MF_RESEARCH_SPOOL_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".local/research-spool"));
            std::fs::create_dir_all(&root)?;
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let path = root.join(format!(
                "{}-{nonce}-{}.json",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            FILES
                .get_or_init(Mutex::default)
                .lock()
                .unwrap()
                .push(path.clone());
            let mut writer = BufWriter::with_capacity(128 * 1024, file);
            writer.write_all(b"[")?;
            Some((path, writer))
        } else {
            None
        };
        Ok(Self {
            memory: Vec::new(),
            disk,
            count: 0,
            bytes_since_advice: 0,
        })
    }
    pub fn push(&mut self, value: Value) -> Result<(), Error> {
        if let Some((_, writer)) = &mut self.disk {
            if self.count != 0 {
                writer.write_all(b",")?;
            }
            let bytes = serde_json::to_vec(&value)?;
            writer.write_all(&bytes)?;
            self.bytes_since_advice += bytes.len() + 1;
            if self.bytes_since_advice >= 8 * 1024 * 1024 {
                writer.flush()?;
                writer.get_ref().sync_data()?;
                advise_cold(writer.get_ref());
                self.bytes_since_advice = 0;
            }
        } else {
            self.memory.push(value);
        }
        self.count += 1;
        Ok(())
    }
    pub fn finish(mut self) -> Result<Value, Error> {
        match self.disk.take() {
            Some((path, mut writer)) => {
                writer.write_all(b"]")?;
                writer.flush()?;
                writer.get_ref().sync_data()?;
                advise_cold(writer.get_ref());
                Ok(json!({(FILE_KEY):path,"count":self.count}))
            }
            None => Ok(Value::Array(self.memory)),
        }
    }
}
// Advice covers only this process's completed files, never global caches.
#[cfg(target_os = "linux")]
fn advise_cold(file: &File) {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
    }
    let _ = unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, 4) };
}
#[cfg(not(target_os = "linux"))]
fn advise_cold(_: &File) {}

pub fn write_result(writer: &mut impl Write, result: &Value) -> Result<(), Error> {
    writer.write_all(b"{")?;
    for (index, (key, value)) in result
        .as_object()
        .ok_or("research result must be an object")?
        .iter()
        .enumerate()
    {
        if index != 0 {
            writer.write_all(b",")?;
        }
        serde_json::to_writer(&mut *writer, key)?;
        writer.write_all(b":")?;
        if matches!(key.as_str(), "trade_receipts" | "book_snapshots")
            && let Some(path) = value.get(FILE_KEY).and_then(Value::as_str)
        {
            let file = File::open(path)?;
            std::io::copy(&mut BufReader::with_capacity(128 * 1024, &file), writer)?;
            advise_cold(&file);
        } else {
            serde_json::to_writer(&mut *writer, value)?;
        }
    }
    writer.write_all(b"}")?;
    Ok(())
}
pub fn write_file(path: &std::path::Path, result: &Value) -> Result<(), Error> {
    let mut writer = BufWriter::with_capacity(128 * 1024, File::create(path)?);
    write_result(&mut writer, result)?;
    writer.flush()?;
    writer.get_ref().sync_data()?;
    advise_cold(writer.get_ref());
    Ok(())
}
struct HashWriter<W> {
    inner: W,
    hash: Sha256,
}
impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
pub fn print_result(result: &Value) -> Result<(), Error> {
    let summary_path = std::env::var_os("MF_RESEARCH_SUMMARY_PATH").map(PathBuf::from);
    if summary_path.as_ref().is_some_and(|p| p.exists()) {
        return Err(
            "research summary already exists; preserve attempts in a fresh directory".into(),
        );
    }
    let stdout = std::io::stdout();
    let mut writer = HashWriter {
        inner: BufWriter::with_capacity(128 * 1024, stdout.lock()),
        hash: Sha256::new(),
    };
    write_result(&mut writer, result)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    if let Some(path) = summary_path {
        let mut summary =
            json!({"schema":1,"receipt_sha256":format!("{:x}",writer.hash.finalize())});
        for key in [
            "steps",
            "clock_step_seconds",
            "simulation_time_ms",
            "trades",
            "run_complete",
            "requested_steps",
        ] {
            if let Some(value) = result.get(key) {
                summary[key] = value.clone();
            }
        }
        if let Some(archives) = ARCHIVES.get() {
            summary["execution_archives"] = json!(archives.lock().unwrap().clone());
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        serde_json::to_writer(&mut file, &summary)?;
        file.sync_all()?;
    }
    // On failure leave partial spools for diagnosis. Delete only our own files.
    if let Some(files) = FILES.get() {
        for path in files.lock().unwrap().drain(..) {
            if let Err(error) = std::fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error.into());
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spooled_arrays_preserve_values_and_order() {
        let mut array = Array::new().unwrap();
        array.push(json!({"a":[1,2],"label":"中文"})).unwrap();
        array.push(Value::Null).unwrap();
        let result = json!({"trade_receipts":array.finish().unwrap(),"steps":2});
        let mut bytes = Vec::new();
        write_result(&mut bytes, &result).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"trade_receipts":[{"a":[1,2],"label":"中文"},null],"steps":2})
        );
        if let Some(path) = result["trade_receipts"]
            .get(FILE_KEY)
            .and_then(Value::as_str)
        {
            std::fs::remove_file(path).unwrap();
        }
    }
}

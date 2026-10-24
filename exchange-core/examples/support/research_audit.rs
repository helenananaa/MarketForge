//! Optional exact per-command evidence, separate from timed performance runs.
use exchange_core::{ActorExecution, GatewayExecution, OrderAction};
use std::io::{BufWriter, Write};

pub struct Audit(BufWriter<std::fs::File>);
impl Audit {
    pub fn open() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        match std::env::var("RESEARCH_ORDER_AUDIT_PATH") {
            Ok(path) => Ok(Some(Self(BufWriter::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            )))),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub fn gateway(
        &mut self,
        action: &OrderAction,
        result: &GatewayExecution,
    ) -> std::io::Result<()> {
        // Debug includes every typed execution field, including actor rejection,
        // engine events, accounting and protection. Compare with the same binary;
        // this research evidence is not a versioned persistence format.
        serde_json::to_writer(
            &mut self.0,
            &serde_json::json!({
                "kind":"gateway", "participant":result.participant_id,
                "action":action, "receipt":format!("{result:?}")
            }),
        )?;
        self.0.write_all(b"\n")
    }
    pub fn authoritative(&mut self, result: &ActorExecution) -> std::io::Result<()> {
        serde_json::to_writer(
            &mut self.0,
            &serde_json::json!({
                "kind":"authoritative", "receipt":format!("{result:?}")
            }),
        )?;
        self.0.write_all(b"\n")
    }
    pub fn finish(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

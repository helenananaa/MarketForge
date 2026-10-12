#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeLoopControl {
    Break,
    Continue,
}

impl RuntimeError {
    pub(crate) fn escaped_loop_control() -> Self {
        Self {
            message: "loop control escaped its enclosing loop".to_owned(),
        }
    }
}

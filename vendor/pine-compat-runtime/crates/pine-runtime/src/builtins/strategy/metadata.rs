use pine_ir::HirCallArg;

use crate::builtins::args::call_arg_expr;
use crate::strategy::{StrategyExitMetadata, StrategyOrderMetadata};
use crate::{HistoricalRuntime, PineValue, RuntimeError};

impl<'a> HistoricalRuntime<'a> {
    pub(super) fn eval_strategy_order_metadata(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<StrategyOrderMetadata, RuntimeError> {
        self.eval_strategy_close_metadata(args, 7)
    }

    pub(super) fn eval_strategy_entry_metadata(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<StrategyOrderMetadata, RuntimeError> {
        Ok(StrategyOrderMetadata {
            comment: self.eval_optional_string_arg(args, 7, "comment")?,
            alert_message: self.eval_optional_string_arg(args, 8, "alert_message")?,
            disable_alert: self.eval_optional_bool_arg(args, 9, "disable_alert")?,
        })
    }

    pub(super) fn eval_strategy_close_metadata(
        &mut self,
        args: &[HirCallArg],
        comment_index: usize,
    ) -> Result<StrategyOrderMetadata, RuntimeError> {
        Ok(StrategyOrderMetadata {
            comment: self.eval_optional_string_arg(args, comment_index, "comment")?,
            alert_message: self.eval_optional_string_arg(
                args,
                comment_index + 1,
                "alert_message",
            )?,
            disable_alert: self.eval_optional_bool_arg(args, comment_index + 2, "disable_alert")?,
        })
    }

    pub(super) fn eval_strategy_exit_metadata(
        &mut self,
        args: &[HirCallArg],
    ) -> Result<StrategyExitMetadata, RuntimeError> {
        Ok(StrategyExitMetadata {
            comment: self.eval_optional_string_arg(args, 12, "comment")?,
            comment_profit: self.eval_optional_string_arg(args, 13, "comment_profit")?,
            comment_loss: self.eval_optional_string_arg(args, 14, "comment_loss")?,
            comment_trailing: self.eval_optional_string_arg(args, 15, "comment_trailing")?,
            alert_message: self.eval_optional_string_arg(args, 16, "alert_message")?,
            alert_profit: self.eval_optional_string_arg(args, 17, "alert_profit")?,
            alert_loss: self.eval_optional_string_arg(args, 18, "alert_loss")?,
            alert_trailing: self.eval_optional_string_arg(args, 19, "alert_trailing")?,
            disable_alert: self.eval_optional_bool_arg(args, 20, "disable_alert")?,
        })
    }

    fn eval_optional_string_arg(
        &mut self,
        args: &[HirCallArg],
        index: usize,
        name: &str,
    ) -> Result<Option<String>, RuntimeError> {
        let Some(expr) = call_arg_expr(args, index, name) else {
            return Ok(None);
        };
        Ok(match self.eval_expr(expr)? {
            PineValue::String(value) => Some(value),
            _ => None,
        })
    }

    pub(super) fn eval_strategy_immediately_arg(
        &mut self,
        args: &[HirCallArg],
        index: usize,
    ) -> Result<bool, RuntimeError> {
        self.eval_optional_bool_arg(args, index, "immediately")
    }

    fn eval_optional_bool_arg(
        &mut self,
        args: &[HirCallArg],
        index: usize,
        name: &str,
    ) -> Result<bool, RuntimeError> {
        let Some(expr) = call_arg_expr(args, index, name) else {
            return Ok(false);
        };
        Ok(matches!(self.eval_expr(expr)?, PineValue::Bool(true)))
    }
}

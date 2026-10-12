use crate::runtime::append_history::AppendHistory;
use crate::{FillGradientSample, HistoricalRuntime, PineValue, RuntimeError};
use pine_ir::{CallSiteId, HirCallArg};

impl HistoricalRuntime<'_> {
    pub(crate) fn eval_gradient_fill(
        &mut self,
        call_site_id: CallSiteId,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let first = self.eval_output_arg(args, 0, "plot1", PineValue::Na)?;
        let second = self.eval_output_arg(args, 1, "plot2", PineValue::Na)?;
        let sample = FillGradientSample {
            top_value: self
                .eval_output_arg(args, 2, "top_value", PineValue::Na)?
                .as_f64()
                .filter(|v| v.is_finite()),
            bottom_value: self
                .eval_output_arg(args, 3, "bottom_value", PineValue::Na)?
                .as_f64()
                .filter(|v| v.is_finite()),
            top_color: gradient_color(self.eval_output_arg(args, 4, "top_color", PineValue::Na)?),
            bottom_color: gradient_color(self.eval_output_arg(
                args,
                5,
                "bottom_color",
                PineValue::Na,
            )?),
        };
        let title = self.eval_output_arg(args, 6, "title", PineValue::String(String::new()))?;
        let editable = self.eval_output_arg(args, 7, "editable", PineValue::Bool(true))?;
        let gaps = self.eval_output_arg(args, 8, "fillgaps", PineValue::Bool(true))?;
        let display = self.eval_output_arg(
            args,
            9,
            "display",
            PineValue::String("display.all".to_owned()),
        )?;
        self.push_fill(
            call_site_id.0,
            first,
            second,
            PineValue::Na,
            title,
            editable,
            PineValue::Na,
            gaps,
            display,
        );
        let bar = self.bars - self.stored_origin;
        if let Some(fill) = self.fills.iter_mut().find(|fill| fill.id == call_site_id.0) {
            let samples = fill.gradient.get_or_insert_with(|| {
                AppendHistory::from_values(std::iter::repeat_n(FillGradientSample::default(), bar))
            });
            while samples.len() < bar {
                samples.push(FillGradientSample::default());
            }
            if samples.len() == bar {
                samples.push(sample);
            } else if let Some(current) = samples.last_mut() {
                *current = sample;
            }
        }
        Ok(PineValue::Void)
    }
}

fn gradient_color(value: PineValue) -> Option<u64> {
    match value {
        PineValue::Color(color) => Some(color),
        _ => None,
    }
}

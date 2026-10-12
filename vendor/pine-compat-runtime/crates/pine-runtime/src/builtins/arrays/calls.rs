use super::*;

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_array_call(
        &mut self,
        opcode: Option<ArrayOpcode>,
        callee: &str,
        args: &[HirCallArg],
    ) -> Option<Result<PineValue, RuntimeError>> {
        Some(match opcode? {
            ArrayOpcode::NewFloat => self.eval_array_new_float(args),
            ArrayOpcode::NewInt => self.eval_array_new_int(args),
            ArrayOpcode::NewBool => self.eval_array_new_bool(args),
            ArrayOpcode::NewString => self.eval_array_new_string(args),
            ArrayOpcode::NewColor => self.eval_array_new_color(args),
            ArrayOpcode::NewLine => self.eval_array_new_line(args),
            ArrayOpcode::NewLineFill => self.eval_array_new_linefill(args),
            ArrayOpcode::NewPolyline => self.eval_array_new_polyline(args),
            ArrayOpcode::NewLabel => self.eval_array_new_label(args),
            ArrayOpcode::NewBox => self.eval_array_new_box(args),
            ArrayOpcode::NewTable => self.eval_array_new_table(args),
            ArrayOpcode::NewChartPoint => self.eval_array_new_chart_point(args),
            ArrayOpcode::NewUserType => {
                let type_name = &callee["array.new<".len()..callee.len() - 1];
                self.eval_array_new_user_type(args, type_name)
            }
            ArrayOpcode::From => self.eval_array_from(args),
            ArrayOpcode::Size => self.eval_array_size(args),
            ArrayOpcode::Push => self.eval_array_push(args),
            ArrayOpcode::Get => self.eval_array_get(args),
            ArrayOpcode::Set => self.eval_array_set(args),
            ArrayOpcode::Insert => self.eval_array_insert(args),
            ArrayOpcode::Pop => self.eval_array_pop(args),
            ArrayOpcode::Remove => self.eval_array_remove(args),
            ArrayOpcode::Shift => self.eval_array_shift(args),
            ArrayOpcode::Unshift => self.eval_array_unshift(args),
            ArrayOpcode::Fill => self.eval_array_fill(args),
            ArrayOpcode::First => self.eval_array_first(args),
            ArrayOpcode::Last => self.eval_array_last(args),
            ArrayOpcode::Copy => self.eval_array_copy(args),
            ArrayOpcode::Slice => self.eval_array_slice(args),
            ArrayOpcode::Concat => self.eval_array_concat(args),
            ArrayOpcode::Includes => self.eval_array_includes(args),
            ArrayOpcode::Every => self.eval_array_truth(args, ArrayTruthMode::Every),
            ArrayOpcode::Some => self.eval_array_truth(args, ArrayTruthMode::Some),
            ArrayOpcode::IndexOf => self.eval_array_indexof(args),
            ArrayOpcode::LastIndexOf => self.eval_array_lastindexof(args),
            ArrayOpcode::BinarySearch => {
                self.eval_array_binary_search(args, ArrayBinarySearchMode::Exact)
            }
            ArrayOpcode::BinarySearchLeftmost => {
                self.eval_array_binary_search(args, ArrayBinarySearchMode::Leftmost)
            }
            ArrayOpcode::BinarySearchRightmost => {
                self.eval_array_binary_search(args, ArrayBinarySearchMode::Rightmost)
            }
            ArrayOpcode::Abs => self.eval_array_abs(args),
            ArrayOpcode::Min => self.eval_array_numeric(args, ArrayNumericMode::Min),
            ArrayOpcode::Max => self.eval_array_numeric(args, ArrayNumericMode::Max),
            ArrayOpcode::Sum => self.eval_array_numeric(args, ArrayNumericMode::Sum),
            ArrayOpcode::Avg => self.eval_array_numeric(args, ArrayNumericMode::Avg),
            ArrayOpcode::Range => self.eval_array_numeric(args, ArrayNumericMode::Range),
            ArrayOpcode::Median => self.eval_array_numeric(args, ArrayNumericMode::Median),
            ArrayOpcode::Mode => self.eval_array_numeric(args, ArrayNumericMode::Mode),
            ArrayOpcode::PercentileNearestRank => {
                self.eval_array_percentile(args, ArrayPercentileMode::NearestRank)
            }
            ArrayOpcode::PercentileLinearInterpolation => {
                self.eval_array_percentile(args, ArrayPercentileMode::LinearInterpolation)
            }
            ArrayOpcode::PercentRank => self.eval_array_percentrank(args),
            ArrayOpcode::Covariance => self.eval_array_covariance(args),
            ArrayOpcode::Standardize => self.eval_array_standardize(args),
            ArrayOpcode::Variance => self.eval_array_variance(args, ArrayVarianceMode::Variance),
            ArrayOpcode::Stdev => self.eval_array_variance(args, ArrayVarianceMode::Stdev),
            ArrayOpcode::Sort => self.eval_array_sort(args),
            ArrayOpcode::SortIndices => self.eval_array_sort_indices(args),
            ArrayOpcode::Reverse => self.eval_array_reverse(args),
            ArrayOpcode::Join => self.eval_array_join(args),
            ArrayOpcode::Clear => self.eval_array_clear(args),
        })
    }
}

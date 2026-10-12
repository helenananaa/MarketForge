use super::{
    BrokerState,
    pending_entries::PendingEntryDirection,
    pending_exits::{ExitQuantityRequest, PendingExitTrigger},
};

impl BrokerState {
    pub(crate) fn place_exit_stop(
        &mut self,
        id: String,
        from_entry: String,
        stop_price: f64,
        bar_index: usize,
    ) {
        self.place_exit_stop_quantity(
            id,
            from_entry,
            stop_price,
            ExitQuantityRequest::Full,
            bar_index,
        );
    }

    pub(crate) fn place_exit_stop_qty(
        &mut self,
        id: String,
        from_entry: String,
        stop_price: f64,
        qty: f64,
        bar_index: usize,
    ) {
        self.place_exit_stop_quantity(
            id,
            from_entry,
            stop_price,
            ExitQuantityRequest::Fixed(qty),
            bar_index,
        );
    }

    pub(crate) fn place_exit_stop_qty_percent(
        &mut self,
        id: String,
        from_entry: String,
        stop_price: f64,
        qty_percent: f64,
        bar_index: usize,
    ) {
        self.place_exit_stop_quantity(
            id,
            from_entry,
            stop_price,
            ExitQuantityRequest::Percent(qty_percent),
            bar_index,
        );
    }

    fn place_exit_stop_quantity(
        &mut self,
        id: String,
        from_entry: String,
        stop_price: f64,
        quantity: ExitQuantityRequest,
        bar_index: usize,
    ) {
        let metadata = self.take_next_exit_metadata();
        let stop_price = self.snap_exit_price(stop_price, false, &from_entry);
        self.place_exit(
            id,
            from_entry,
            PendingExitTrigger::Stop(stop_price),
            quantity,
            bar_index,
            metadata,
        );
    }

    pub(crate) fn place_exit_limit(
        &mut self,
        id: String,
        from_entry: String,
        limit_price: f64,
        bar_index: usize,
    ) {
        self.place_exit_limit_quantity(
            id,
            from_entry,
            limit_price,
            ExitQuantityRequest::Full,
            bar_index,
        );
    }

    pub(crate) fn place_exit_limit_qty(
        &mut self,
        id: String,
        from_entry: String,
        limit_price: f64,
        qty: f64,
        bar_index: usize,
    ) {
        self.place_exit_limit_quantity(
            id,
            from_entry,
            limit_price,
            ExitQuantityRequest::Fixed(qty),
            bar_index,
        );
    }

    pub(crate) fn place_exit_limit_qty_percent(
        &mut self,
        id: String,
        from_entry: String,
        limit_price: f64,
        qty_percent: f64,
        bar_index: usize,
    ) {
        self.place_exit_limit_quantity(
            id,
            from_entry,
            limit_price,
            ExitQuantityRequest::Percent(qty_percent),
            bar_index,
        );
    }

    fn place_exit_limit_quantity(
        &mut self,
        id: String,
        from_entry: String,
        limit_price: f64,
        quantity: ExitQuantityRequest,
        bar_index: usize,
    ) {
        let metadata = self.take_next_exit_metadata();
        let limit_price = self.snap_exit_price(limit_price, true, &from_entry);
        self.place_exit(
            id,
            from_entry,
            PendingExitTrigger::Limit(limit_price),
            quantity,
            bar_index,
            metadata,
        );
    }

    fn snap_exit_price(&self, price: f64, is_limit: bool, from_entry: &str) -> f64 {
        let Some(tick) = self.price_tick else {
            return price;
        };
        if !price.is_finite() {
            return price;
        }
        let is_long =
            if self.position_size != 0.0 && self.open_position_size_for_entry(from_entry) > 0.0 {
                self.position_size > 0.0
            } else if let Some(pending_entry) = self.order_book.entries().find_by_id(from_entry) {
                pending_entry.direction == PendingEntryDirection::Long
            } else if self.position_size != 0.0 {
                self.position_size > 0.0
            } else {
                return price;
            };
        let ticks = price / tick;
        if !ticks.is_finite() {
            return price;
        }
        let nearest = ticks.round();
        if (ticks - nearest).abs() <= 1e-8 {
            // Canonicalize values already on the chart grid: an average entry
            // price can be a few ULPs above a touched high/low at that tick.
            return canonical_tick_price(nearest, tick);
        }
        // Limits round toward a favorable price, stops toward an adverse one.
        let round_up = is_long == is_limit;
        let aligned = if round_up {
            ticks.ceil()
        } else {
            ticks.floor()
        };
        canonical_tick_price(aligned, tick)
    }

    pub(crate) fn place_exit_bracket(
        &mut self,
        id: String,
        from_entry: String,
        downside_price: f64,
        upside_price: f64,
        bar_index: usize,
    ) {
        self.place_exit_bracket_quantity(
            id,
            from_entry,
            downside_price,
            upside_price,
            ExitQuantityRequest::Full,
            bar_index,
        );
    }

    pub(crate) fn place_exit_bracket_qty(
        &mut self,
        id: String,
        from_entry: String,
        downside_price: f64,
        upside_price: f64,
        qty: f64,
        bar_index: usize,
    ) {
        self.place_exit_bracket_quantity(
            id,
            from_entry,
            downside_price,
            upside_price,
            ExitQuantityRequest::Fixed(qty),
            bar_index,
        );
    }

    pub(crate) fn place_exit_bracket_qty_percent(
        &mut self,
        id: String,
        from_entry: String,
        downside_price: f64,
        upside_price: f64,
        qty_percent: f64,
        bar_index: usize,
    ) {
        self.place_exit_bracket_quantity(
            id,
            from_entry,
            downside_price,
            upside_price,
            ExitQuantityRequest::Percent(qty_percent),
            bar_index,
        );
    }

    fn place_exit_bracket_quantity(
        &mut self,
        id: String,
        from_entry: String,
        downside_price: f64,
        upside_price: f64,
        quantity: ExitQuantityRequest,
        bar_index: usize,
    ) {
        let metadata = self.take_next_exit_metadata();
        let downside_price = self.snap_exit_price(downside_price, false, &from_entry);
        let upside_price = self.snap_exit_price(upside_price, true, &from_entry);
        self.place_exit(
            id,
            from_entry,
            PendingExitTrigger::Bracket {
                downside: downside_price,
                upside: upside_price,
            },
            quantity,
            bar_index,
            metadata,
        );
    }
}

pub(super) fn canonical_tick_price(ticks: f64, tick: f64) -> f64 {
    // Chart OHLC is parsed from decimal prices. Multiplying a tick count by
    // a binary approximation of the tick can land one ULP past an exact high
    // or low, making a touched order look untouched. Reconstruct decimal
    // ticks through their integer numerator and decimal scale when possible.
    for decimals in 0..=12 {
        let scale = 10_f64.powi(decimals);
        let numerator = tick * scale;
        let rounded = numerator.round();
        if rounded >= 1.0
            && (numerator - rounded).abs() <= 16.0 * f64::EPSILON * numerator.abs().max(1.0)
        {
            let price = ticks * rounded / scale;
            if price.is_finite() {
                return price;
            }
        }
    }
    ticks * tick
}

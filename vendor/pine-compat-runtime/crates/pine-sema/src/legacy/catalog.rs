use super::PineDialect;

pub const LEGACY_TRANSLATOR_REVISION: u32 = 38;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LegacyRuleKind {
    ExactFunctionAlias,
    ExactSymbolAlias,
    FocusedCall,
    FocusedExpression,
    FocusedInput,
    FocusedInputConstant,
    FocusedOutput,
    FocusedSecurity,
    FocusedDeclaration,
}

impl LegacyRuleKind {
    #[must_use]
    pub const fn is_call(self) -> bool {
        !matches!(self, Self::ExactSymbolAlias | Self::FocusedInputConstant)
    }

    #[must_use]
    pub const fn is_exact(self) -> bool {
        matches!(self, Self::ExactFunctionAlias | Self::ExactSymbolAlias)
    }

    #[must_use]
    pub const fn is_value(self) -> bool {
        matches!(self, Self::ExactSymbolAlias | Self::FocusedInputConstant)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRuleSupport {
    Supported,
    UnsupportedKnown { reason: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyRule {
    pub source_name: &'static str,
    pub canonical_name: Option<&'static str>,
    pub min_version: PineDialect,
    pub max_version: PineDialect,
    pub kind: LegacyRuleKind,
    pub support: LegacyRuleSupport,
}

impl LegacyRule {
    #[must_use]
    pub const fn applies_to(self, dialect: PineDialect) -> bool {
        dialect.version() >= self.min_version.version()
            && dialect.version() <= self.max_version.version()
    }
}

const fn exact_symbol(
    source_name: &'static str,
    canonical_name: &'static str,
    min_version: PineDialect,
    max_version: PineDialect,
) -> LegacyRule {
    LegacyRule {
        source_name,
        canonical_name: Some(canonical_name),
        min_version,
        max_version,
        kind: LegacyRuleKind::ExactSymbolAlias,
        support: LegacyRuleSupport::Supported,
    }
}

const fn focused_input_constant(
    source_name: &'static str,
    canonical_name: &'static str,
    min_version: PineDialect,
    max_version: PineDialect,
) -> LegacyRule {
    LegacyRule {
        source_name,
        canonical_name: Some(canonical_name),
        min_version,
        max_version,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    }
}

pub const LEGACY_RULES: &[LegacyRule] = &[
    LegacyRule {
        source_name: "abs",
        canonical_name: Some("math.abs"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "acos",
        canonical_name: Some("math.acos"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "alma",
        canonical_name: Some("ta.alma"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("aqua", "color.aqua", PineDialect::V1, PineDialect::V3),
    exact_symbol("area", "plot.style_area", PineDialect::V1, PineDialect::V3),
    exact_symbol(
        "areabr",
        "plot.style_areabr",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "asin",
        canonical_name: Some("math.asin"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "atan",
        canonical_name: Some("math.atan"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "atr",
        canonical_name: Some("ta.atr"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "avg",
        canonical_name: Some("math.avg"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "barcolor",
        canonical_name: Some("barcolor"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "barcolor",
        canonical_name: Some("barcolor"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "barssince",
        canonical_name: Some("ta.barssince"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "bb",
        canonical_name: Some("ta.bb"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "bgcolor",
        canonical_name: Some("bgcolor"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "bgcolor",
        canonical_name: Some("bgcolor"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("black", "color.black", PineDialect::V1, PineDialect::V3),
    exact_symbol("blue", "color.blue", PineDialect::V1, PineDialect::V3),
    focused_input_constant("bool", "input.bool", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "cci",
        canonical_name: Some("ta.cci"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "ceil",
        canonical_name: Some("math.ceil"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "change",
        canonical_name: Some("ta.change"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "circles",
        "plot.style_circles",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "color",
        canonical_name: Some("color.new"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "columns",
        "plot.style_columns",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "cos",
        canonical_name: Some("math.cos"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "cross",
        "plot.style_cross",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "cross",
        canonical_name: Some("ta.cross"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "crossover",
        canonical_name: Some("ta.crossover"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "crossunder",
        canonical_name: Some("ta.crossunder"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "dashed",
        "hline.style_dashed",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol(
        "dotted",
        "hline.style_dotted",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "ema",
        canonical_name: Some("ta.ema"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "exp",
        canonical_name: Some("math.exp"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "falling",
        canonical_name: Some("ta.falling"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "fill",
        canonical_name: Some("fill"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "fill",
        canonical_name: Some("fill"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    focused_input_constant("float", "input.float", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "floor",
        canonical_name: Some("math.floor"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "friday",
        "dayofweek.friday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol("fuchsia", "color.fuchsia", PineDialect::V1, PineDialect::V3),
    exact_symbol("gray", "color.gray", PineDialect::V1, PineDialect::V3),
    exact_symbol("green", "color.green", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "heikinashi",
        canonical_name: Some("ticker.heikinashi"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "highest",
        canonical_name: Some("ta.highest"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "histogram",
        "plot.style_histogram",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "hline",
        canonical_name: Some("hline"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V2,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "hline",
        canonical_name: Some("hline"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "hline",
        canonical_name: Some("hline"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "hma",
        canonical_name: Some("ta.hma"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "iff",
        canonical_name: None,
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedExpression,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input",
        canonical_name: Some("input"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V2,
        kind: LegacyRuleKind::FocusedInput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input",
        canonical_name: Some("input"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedInput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input",
        canonical_name: Some("input"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.bool",
        canonical_name: Some("input.bool"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.color",
        canonical_name: Some("input.color"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.float",
        canonical_name: Some("input.float"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.integer",
        canonical_name: Some("input.int"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.price",
        canonical_name: Some("input.price"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.resolution",
        canonical_name: Some("input.timeframe"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.session",
        canonical_name: Some("input.session"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.source",
        canonical_name: Some("input.source"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.string",
        canonical_name: Some("input.string"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.symbol",
        canonical_name: Some("input.symbol"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "input.time",
        canonical_name: Some("input.time"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedInputConstant,
        support: LegacyRuleSupport::Supported,
    },
    focused_input_constant("integer", "input.int", PineDialect::V1, PineDialect::V3),
    exact_symbol(
        "interval",
        "timeframe.multiplier",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol(
        "isdaily",
        "timeframe.isdaily",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol("isdwm", "timeframe.isdwm", PineDialect::V1, PineDialect::V3),
    exact_symbol(
        "isintraday",
        "timeframe.isintraday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol(
        "isminutes",
        "timeframe.isminutes",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol(
        "ismonthly",
        "timeframe.ismonthly",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol(
        "isseconds",
        "timeframe.isseconds",
        PineDialect::V3,
        PineDialect::V3,
    ),
    exact_symbol(
        "isweekly",
        "timeframe.isweekly",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol(
        "label.style_labeldown",
        "label.style_label_down",
        PineDialect::V4,
        PineDialect::V4,
    ),
    exact_symbol(
        "label.style_labelup",
        "label.style_label_up",
        PineDialect::V4,
        PineDialect::V4,
    ),
    exact_symbol("lime", "color.lime", PineDialect::V1, PineDialect::V3),
    exact_symbol("line", "plot.style_line", PineDialect::V1, PineDialect::V3),
    exact_symbol(
        "linebr",
        "plot.style_linebr",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "linreg",
        canonical_name: Some("ta.linreg"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "log",
        canonical_name: Some("math.log"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "log10",
        canonical_name: Some("math.log10"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "lowest",
        canonical_name: Some("ta.lowest"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "macd",
        canonical_name: Some("ta.macd"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("maroon", "color.maroon", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "max",
        canonical_name: Some("math.max"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "mfi",
        canonical_name: Some("ta.mfi"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "min",
        canonical_name: Some("math.min"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "mom",
        canonical_name: Some("ta.mom"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "monday",
        "dayofweek.monday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol("n", "bar_index", PineDialect::V1, PineDialect::V3),
    exact_symbol("navy", "color.navy", PineDialect::V1, PineDialect::V3),
    exact_symbol("obv", "ta.obv", PineDialect::V1, PineDialect::V4),
    LegacyRule {
        source_name: "offset",
        canonical_name: None,
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedExpression,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("olive", "color.olive", PineDialect::V1, PineDialect::V3),
    exact_symbol("orange", "color.orange", PineDialect::V1, PineDialect::V3),
    exact_symbol(
        "period",
        "timeframe.period",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "pivothigh",
        canonical_name: Some("ta.pivothigh"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "pivotlow",
        canonical_name: Some("ta.pivotlow"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plot",
        canonical_name: Some("plot"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V2,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plot",
        canonical_name: Some("plot"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plot",
        canonical_name: Some("plot"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V5,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotarrow",
        canonical_name: Some("plotarrow"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotarrow",
        canonical_name: Some("plotarrow"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotbar",
        canonical_name: Some("plotbar"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotbar",
        canonical_name: Some("plotbar"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotcandle",
        canonical_name: Some("plotcandle"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotcandle",
        canonical_name: Some("plotcandle"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotchar",
        canonical_name: Some("plotchar"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotchar",
        canonical_name: Some("plotchar"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotshape",
        canonical_name: Some("plotshape"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "plotshape",
        canonical_name: Some("plotshape"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedOutput,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "pow",
        canonical_name: Some("math.pow"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("purple", "color.purple", PineDialect::V1, PineDialect::V3),
    exact_symbol("red", "color.red", PineDialect::V1, PineDialect::V3),
    focused_input_constant(
        "resolution",
        "input.timeframe",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "rising",
        canonical_name: Some("ta.rising"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "rma",
        canonical_name: Some("ta.rma"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "round",
        canonical_name: Some("math.round"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "rsi",
        canonical_name: Some("ta.rsi"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedCall,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "saturday",
        "dayofweek.saturday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "security",
        canonical_name: Some("request.security"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedSecurity,
        support: LegacyRuleSupport::Supported,
    },
    focused_input_constant("session", "input.session", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "sign",
        canonical_name: Some("math.sign"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("silver", "color.silver", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "sma",
        canonical_name: Some("ta.sma"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "solid",
        "hline.style_solid",
        PineDialect::V1,
        PineDialect::V3,
    ),
    focused_input_constant("source", "input.source", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "sqrt",
        canonical_name: Some("math.sqrt"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "stdev",
        canonical_name: Some("ta.stdev"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "stepline",
        "plot.style_stepline",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "stoch",
        canonical_name: Some("ta.stoch"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    focused_input_constant("string", "input.string", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "study",
        canonical_name: Some("indicator"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V2,
        kind: LegacyRuleKind::FocusedDeclaration,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "study",
        canonical_name: Some("indicator"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V3,
        kind: LegacyRuleKind::FocusedDeclaration,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "study",
        canonical_name: Some("indicator"),
        min_version: PineDialect::V4,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedDeclaration,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "sum",
        canonical_name: Some("math.sum"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "sunday",
        "dayofweek.sunday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    focused_input_constant("symbol", "input.symbol", PineDialect::V1, PineDialect::V3),
    exact_symbol("teal", "color.teal", PineDialect::V1, PineDialect::V3),
    exact_symbol(
        "thursday",
        "dayofweek.thursday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol("ticker", "syminfo.ticker", PineDialect::V1, PineDialect::V3),
    exact_symbol(
        "tickerid",
        "syminfo.tickerid",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "tostring",
        canonical_name: Some("str.tostring"),
        min_version: PineDialect::V3,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedCall,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("tr", "ta.tr", PineDialect::V1, PineDialect::V4),
    LegacyRule {
        source_name: "tr",
        canonical_name: Some("ta.tr"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "tuesday",
        "dayofweek.tuesday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    LegacyRule {
        source_name: "valuewhen",
        canonical_name: Some("ta.valuewhen"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("vwap", "ta.vwap", PineDialect::V1, PineDialect::V4),
    LegacyRule {
        source_name: "vwap",
        canonical_name: Some("ta.vwap"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::FocusedCall,
        support: LegacyRuleSupport::Supported,
    },
    LegacyRule {
        source_name: "vwma",
        canonical_name: Some("ta.vwma"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol(
        "wednesday",
        "dayofweek.wednesday",
        PineDialect::V1,
        PineDialect::V3,
    ),
    exact_symbol("white", "color.white", PineDialect::V1, PineDialect::V3),
    LegacyRule {
        source_name: "wma",
        canonical_name: Some("ta.wma"),
        min_version: PineDialect::V1,
        max_version: PineDialect::V4,
        kind: LegacyRuleKind::ExactFunctionAlias,
        support: LegacyRuleSupport::Supported,
    },
    exact_symbol("yellow", "color.yellow", PineDialect::V1, PineDialect::V3),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogValidationError(pub(crate) String);

pub(crate) fn matching_rules(
    rules: &'static [LegacyRule],
    source_name: &str,
) -> &'static [LegacyRule] {
    let start = rules.partition_point(|rule| rule.source_name < source_name);
    let end = rules[start..].partition_point(|rule| rule.source_name == source_name) + start;
    &rules[start..end]
}

pub(crate) fn validate_catalog(rules: &[LegacyRule]) -> Vec<CatalogValidationError> {
    let mut errors = Vec::new();
    for pair in rules.windows(2) {
        if pair[0].source_name > pair[1].source_name {
            errors.push(CatalogValidationError(format!(
                "catalog is not sorted: `{}` appears before `{}`",
                pair[0].source_name, pair[1].source_name
            )));
        }
    }

    for (index, rule) in rules.iter().enumerate() {
        if rule.min_version > rule.max_version {
            errors.push(CatalogValidationError(format!(
                "invalid version range for `{}`",
                rule.source_name
            )));
        }
        if rule.max_version > PineDialect::V4
            && !(rule.source_name == "plot"
                && rule.kind == LegacyRuleKind::FocusedOutput
                && rule.max_version == PineDialect::V5)
        {
            errors.push(CatalogValidationError(format!(
                "legacy rule `{}` leaks into modern dialects",
                rule.source_name
            )));
        }
        match (rule.kind, rule.support, rule.canonical_name) {
            (LegacyRuleKind::ExactFunctionAlias, LegacyRuleSupport::Supported, Some(canonical))
                if pine_builtins::get_phase_1_builtin(canonical).is_none() =>
            {
                errors.push(CatalogValidationError(format!(
                    "canonical function `{canonical}` for `{}` is not registered",
                    rule.source_name
                )));
            }
            (LegacyRuleKind::ExactSymbolAlias, LegacyRuleSupport::Supported, Some(canonical))
                if !canonical_symbol_exists(canonical) =>
            {
                errors.push(CatalogValidationError(format!(
                    "canonical symbol `{canonical}` for `{}` is not registered",
                    rule.source_name
                )));
            }
            (kind, LegacyRuleSupport::Supported, None) if kind.is_exact() => {
                errors.push(CatalogValidationError(format!(
                    "supported exact rule `{}` has no canonical target",
                    rule.source_name
                )));
            }
            (LegacyRuleKind::FocusedDeclaration, LegacyRuleSupport::Supported, Some(canonical))
                if pine_builtins::get_phase_1_builtin(canonical).is_none() =>
            {
                errors.push(CatalogValidationError(format!(
                    "canonical declaration `{canonical}` for `{}` is not registered",
                    rule.source_name
                )));
            }
            (LegacyRuleKind::FocusedDeclaration, LegacyRuleSupport::Supported, None) => {
                errors.push(CatalogValidationError(format!(
                    "supported focused declaration `{}` has no canonical target",
                    rule.source_name
                )));
            }
            (
                LegacyRuleKind::FocusedInput
                | LegacyRuleKind::FocusedInputConstant
                | LegacyRuleKind::FocusedOutput
                | LegacyRuleKind::FocusedCall
                | LegacyRuleKind::FocusedSecurity,
                LegacyRuleSupport::Supported,
                Some(canonical),
            ) if pine_builtins::get_phase_1_builtin(canonical).is_none() => {
                errors.push(CatalogValidationError(format!(
                    "canonical focused target `{canonical}` for `{}` is not registered",
                    rule.source_name
                )));
            }
            (
                LegacyRuleKind::FocusedInput
                | LegacyRuleKind::FocusedInputConstant
                | LegacyRuleKind::FocusedOutput
                | LegacyRuleKind::FocusedCall
                | LegacyRuleKind::FocusedSecurity,
                LegacyRuleSupport::Supported,
                None,
            ) => {
                errors.push(CatalogValidationError(format!(
                    "supported focused rule `{}` has no canonical target",
                    rule.source_name
                )));
            }
            (kind, LegacyRuleSupport::Supported, _)
                if !kind.is_exact()
                    && !matches!(
                        kind,
                        LegacyRuleKind::FocusedDeclaration
                            | LegacyRuleKind::FocusedCall
                            | LegacyRuleKind::FocusedExpression
                            | LegacyRuleKind::FocusedInput
                            | LegacyRuleKind::FocusedInputConstant
                            | LegacyRuleKind::FocusedOutput
                            | LegacyRuleKind::FocusedSecurity
                    ) =>
            {
                errors.push(CatalogValidationError(format!(
                    "focused rule `{}` cannot use exact lowering",
                    rule.source_name
                )));
            }
            _ => {}
        }

        for other in rules
            .iter()
            .skip(index + 1)
            .take_while(|other| other.source_name == rule.source_name)
            .filter(|other| other.kind == rule.kind)
        {
            if rule.min_version <= other.max_version && other.min_version <= rule.max_version {
                errors.push(CatalogValidationError(format!(
                    "overlapping {:?} rules for `{}`",
                    rule.kind, rule.source_name
                )));
            }
        }
    }
    errors
}

fn canonical_symbol_exists(name: &str) -> bool {
    pine_builtins::named_color(name).is_some()
        || pine_builtins::builtin_series_value_type(name).is_some()
        || pine_builtins::named_float_constant(name).is_some()
        || pine_builtins::named_int_constant(name).is_some()
        || pine_builtins::named_string_constant(name).is_some()
}

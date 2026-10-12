// Internal set representation, not TradingView's numeric enum encoding.
// Native all remains distinct even from all five currently named locations.
const ALL: u8 = 63;
const OTHER: u8 = 32;
const LOCATIONS: [(&str, u8); 5] = [
    ("display.pane", 1),
    ("display.price_scale", 2),
    ("display.status_line", 4),
    ("display.data_window", 8),
    ("display.pine_screener", 16),
];

fn atom(value: &str) -> Option<u8> {
    match value {
        "display.all" => Some(ALL),
        "display.none" => Some(0),
        _ => LOCATIONS
            .iter()
            .find(|(name, _)| *name == value)
            .map(|(_, bit)| *bit),
    }
}

fn parse(value: &str) -> Option<u8> {
    let mut parts = value.split(['+', '-']);
    let mut result = atom(parts.next()?)?;
    for (op, part) in value.chars().filter(|c| matches!(c, '+' | '-')).zip(parts) {
        let rhs = atom(part)?;
        result = if op == '+' {
            result | rhs
        } else {
            result & !rhs
        };
    }
    Some(result)
}

fn render(value: u8) -> String {
    if value == ALL {
        return "display.all".into();
    }
    if value == 0 {
        return "display.none".into();
    }
    if value & OTHER != 0 {
        let mut result = String::from("display.all");
        for (name, bit) in LOCATIONS {
            if value & bit == 0 {
                result.push('-');
                result.push_str(name);
            }
        }
        return result;
    }
    LOCATIONS
        .iter()
        .filter(|(_, bit)| value & bit != 0)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join("+")
}

pub fn combine_display_values(left: &str, right: &str, subtract: bool) -> Option<String> {
    let left = parse(left)?;
    let right = parse(right)?;
    Some(render(if subtract {
        left & !right
    } else {
        left | right
    }))
}

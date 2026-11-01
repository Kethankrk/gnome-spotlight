//! Inline calculation: arithmetic, percentages and unit conversion (ticket 06).
//!
//! Recognition is keyword-free — nobody types `calc:` — and deliberately conservative: the whole
//! input must parse, so an ordinary search is never mistaken for an expression and never
//! interrupted by an error row. [`evaluate`] answers `None` for anything it cannot read, and the
//! calculator provider then contributes nothing at all.
//!
//! What is recognised:
//!
//! * arithmetic over integers and decimals with `+ - * /`, parentheses, unary signs and the
//!   unicode `× ÷ −` spellings;
//! * a trailing `%` as a postfix "divide by a hundred" (`50%` is `0.5`) and `of` as a
//!   multiplication, so `20% of 50` is `10`;
//! * a unit conversion — `10 km in miles`, `10km to mi`, `1 in cm` — across length, mass,
//!   temperature and volume.
//!
//! Deliberately absent, per the spec's "Out of Scope": currency, functions, variables, modulo,
//! exponentiation, feet-and-inches notation and any word that is not a unit or a joiner.

mod expr;
mod units;

/// The longest input worth reading as an expression. Ordinary queries are far shorter, and the
/// cap keeps a pasted page of prose out of the parser.
const MAX_INPUT_CHARS: usize = 128;

/// How many decimal places a value is shown with before rounding.
const DECIMALS: usize = 6;

/// A recognised calculation, ready for the result row.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    /// The input, normalised: trimmed, inner whitespace collapsed, lowercased. It is the row's
    /// id suffix and its preview `expression`.
    pub expression: String,
    /// The value, in the unit named by [`Evaluation::unit`].
    pub value: f64,
    /// The canonical symbol of the value's unit, or `""` for a plain number.
    pub unit: String,
}

impl Evaluation {
    /// The value as the overlay shows it: rounded, without trailing zeros.
    pub fn value_text(&self) -> String {
        format_value(self.value)
    }

    /// The value with its unit, when it has one: `4`, `6.213712 mi`.
    pub fn display(&self) -> String {
        if self.unit.is_empty() {
            self.value_text()
        } else {
            format!("{} {}", self.value_text(), self.unit)
        }
    }
}

/// Read `input` as a calculation, or `None` when it is not one.
///
/// Unit conversions are tried first: their words make arithmetic fail anyway, and a formula such
/// as `1 in cm` is only ever a conversion.
pub fn evaluate(input: &str) -> Option<Evaluation> {
    if input.chars().count() > MAX_INPUT_CHARS {
        return None;
    }
    let expression = normalise(input);
    if expression.is_empty() {
        return None;
    }
    if let Some(conversion) = units::convert(&expression) {
        return Some(conversion);
    }
    expr::evaluate(&expression).map(|value| Evaluation {
        expression,
        value,
        unit: String::new(),
    })
}

/// Trim, collapse inner whitespace and lowercase, so that equivalent input produces one id.
pub fn normalise(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Render a value the way the overlay shows it.
///
/// [`evaluate`] never yields a non-finite value; the fallback exists so this stays total.
pub fn format_value(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    let magnitude = value.abs();
    if !value.is_finite() || magnitude >= 1e15 || magnitude < 1e-6 {
        return format!("{value:e}");
    }
    if magnitude >= 1e6 {
        return format!("{value}");
    }
    let mut text = format!("{value:.DECIMALS$}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expressions the calculator must read, and the value each one has.
    const ARITHMETIC: &[(&str, f64)] = &[
        ("0", 0.0),
        ("42", 42.0),
        ("2+2", 4.0),
        ("2 + 2", 4.0),
        ("7 - 9", -2.0),
        ("2 * 3", 6.0),
        ("2 × 3", 6.0),
        ("8 ÷ 2", 4.0),
        ("6 / 4", 1.5),
        ("1 / 4", 0.25),
        ("2 + 3 * 4", 14.0),
        ("(2 + 3) * 4", 20.0),
        ("2 * (3 + 4) - 5 / 5", 13.0),
        ("10 - 2 - 3", 5.0),
        ("100 / 5 / 2", 10.0),
        ("((1 + 2) * (3 + 4))", 21.0),
        ("-5", -5.0),
        ("3 - -2", 5.0),
        ("-(2 + 3)", -5.0),
        ("+7", 7.0),
        ("1.5 + 2.25", 3.75),
        (".5 * 4", 2.0),
        ("1e3 + 1", 1001.0),
        ("2 * -3", -6.0),
        ("--3", 3.0),
        ("50%", 0.5),
        ("50 %", 0.5),
        ("100%", 1.0),
        ("20% of 50", 10.0),
        ("200 * 10%", 20.0),
        ("10 + 5%", 10.05),
        ("(1 + 1)% of (10 * 20)", 4.0),
    ];

    #[test]
    fn arithmetic_evaluates() {
        for (input, expected) in ARITHMETIC {
            let evaluation = evaluate(input).unwrap_or_else(|| panic!("`{input}` must parse"));
            assert_eq!(evaluation.unit, "", "`{input}` is not a conversion");
            assert!(
                (evaluation.value - expected).abs() < 1e-9,
                "`{input}` evaluated to {}, want {expected}",
                evaluation.value
            );
        }
    }

    /// Text that must never become a calculation (stories 28 and 29).
    const REJECTED: &[&str] = &[
        "",
        "   ",
        "nautilus",
        "hello world",
        "abc 123",
        "the 2nd",
        "3 apples",
        "two plus two",
        "2 + abc",
        "km",
        "c",
        "10 km",
        "km in miles",
        "10 km in bananas",
        "10 km in kg",
        "10 km in",
        "10 km in miles extra",
        "10 in",
        "5 ft 2 in",
        "2 hours in minutes",
        "100 usd in eur",
        "1 lightyear in km",
        "2 +",
        "+",
        "%",
        "(2 + 3",
        "2 + 3)",
        "((",
        "2)(3",
        "(2)(3)",
        "2 ** 3",
        "5 % 2",
        "1..2",
        "1e",
        "2:30",
        "1/0",
        "0/0",
        "1,000 + 5",
        "3 x 4",
        "###",
        "2 + 2 = 4",
        "√4",
        "fils 2",
    ];

    #[test]
    fn ordinary_text_is_never_a_calculation() {
        for input in REJECTED {
            assert!(
                evaluate(input).is_none(),
                "`{input}` must not be read as an expression"
            );
        }
    }

    /// Conversions the calculator must read: input, value, canonical unit symbol.
    const CONVERSIONS: &[(&str, f64, &str)] = &[
        ("10 km in miles", 6.213_711_922_373_339, "mi"),
        ("10km in miles", 6.213_711_922_373_339, "mi"),
        ("10 km to mi", 6.213_711_922_373_339, "mi"),
        ("10 kilometres in miles", 6.213_711_922_373_339, "mi"),
        ("1 mile in km", 1.609_344, "km"),
        ("1 m in cm", 100.0, "cm"),
        ("3 ft in inches", 36.0, "in"),
        ("1 in cm", 2.54, "cm"),
        ("12 in ft", 1.0, "ft"),
        ("26.2 miles in km", 42.164_812_8, "km"),
        ("500 g in kg", 0.5, "kg"),
        ("1 kg in lb", 2.204_622_621_848_775_7, "lb"),
        ("16 oz in lb", 1.0, "lb"),
        ("1 stone in kg", 6.350_293_18, "kg"),
        ("100 c in f", 212.0, "°F"),
        ("212 f in c", 100.0, "°C"),
        ("0 c in k", 273.15, "K"),
        ("273.15 k in c", 0.0, "°C"),
        ("-40 c in f", -40.0, "°F"),
        ("37 celsius in fahrenheit", 98.600_000_000_000_01, "°F"),
        ("1 l in ml", 1000.0, "mL"),
        ("1 gal in l", 3.785_411_784, "L"),
        ("2 cups in tbsp", 32.0, "tbsp"),
        ("1 tbsp in tsp", 3.0, "tsp"),
    ];

    #[test]
    fn conversions_convert() {
        for (input, expected, unit) in CONVERSIONS {
            let evaluation = evaluate(input).unwrap_or_else(|| panic!("`{input}` must parse"));
            assert_eq!(evaluation.unit, *unit, "`{input}` unit");
            let tolerance = 1e-6 * expected.abs().max(1.0);
            assert!(
                (evaluation.value - expected).abs() <= tolerance,
                "`{input}` converted to {}, want {expected}",
                evaluation.value
            );
        }
    }

    #[test]
    fn conversions_round_trip_through_the_displayed_value() {
        for (value, from, to) in [
            (10.0, "km", "miles"),
            (1.0, "kg", "lb"),
            (2.5, "l", "gal"),
            (100.0, "c", "f"),
            (37.0, "f", "c"),
            (1.0, "m", "in"),
            (500.0, "g", "oz"),
        ] {
            let there = evaluate(&format!("{value} {from} in {to}")).expect("forward conversion");
            let back = evaluate(&format!("{} {to} in {from}", there.value_text()))
                .expect("reverse conversion");
            assert!(
                (back.value - value).abs() < 1e-5 * value.abs().max(1.0),
                "{value} {from} -> {} {to} -> {} {from}",
                there.value_text(),
                back.value_text()
            );
        }
    }

    #[test]
    fn an_expression_is_normalised_so_equivalent_input_shares_one_id() {
        assert_eq!(normalise("  2   +   2 "), "2 + 2");
        assert_eq!(
            evaluate(" 2 + 2 ").expect("parses").expression,
            "2 + 2",
            "whitespace collapses"
        );
        assert_eq!(
            evaluate("10  KM in  Miles").expect("parses").expression,
            "10 km in miles",
            "units are lowercased, so the id is stable"
        );
    }

    #[test]
    fn values_are_rendered_without_noise() {
        assert_eq!(format_value(4.0), "4");
        assert_eq!(format_value(-40.0), "-40");
        assert_eq!(format_value(3.75), "3.75");
        assert_eq!(format_value(0.5), "0.5");
        assert_eq!(format_value(10.0 / 3.0), "3.333333");
        assert_eq!(format_value(6.213_711_922_373_339), "6.213712");
        assert_eq!(format_value(1_234_567.891), "1234567.891");
        assert_eq!(format_value(1e20), "1e20");
        assert_eq!(format_value(5e-7), "5e-7");
        assert_eq!(format_value(0.0), "0");
        assert_eq!(format_value(-0.0), "0");
    }

    #[test]
    fn an_evaluation_carries_its_display_forms() {
        let sum = evaluate("2 + 2").expect("parses");
        assert_eq!(sum.value_text(), "4");
        assert_eq!(sum.display(), "4");

        let conversion = evaluate("10 km in miles").expect("parses");
        assert_eq!(conversion.value_text(), "6.213712");
        assert_eq!(conversion.display(), "6.213712 mi");
    }

    #[test]
    fn an_overlong_input_is_not_read_as_an_expression() {
        let long = "1+1+".repeat(40);
        assert!(evaluate(&long).is_none());
    }
}

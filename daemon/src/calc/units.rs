//! Unit conversion across length, mass, temperature and volume.
//!
//! A conversion is read as `<number> <unit> [<in|to>] <unit>`: the number may be glued to the
//! first unit (`10km`, `-40°c`), the joiner may be left out when the two words are units of the
//! same category (`1 in cm`), and a category mismatch is simply not a conversion. Nothing else is
//! a conversion: a lone `10 km` has no target and a lone `km` has no number.
//!
//! Currency, time, data and astronomical units are deliberately absent — the spec's "Out of
//! Scope" defers currency, and inventing the rest would start to hijack ordinary searches.

use super::expr;
use super::Evaluation;

/// The families a conversion may move within. A conversion never crosses categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Length,
    Mass,
    Temperature,
    Volume,
}

/// One unit: the words that name it, the symbol it is shown with, its category, and how many base
/// units it is worth (metre, gram, litre — ignored for temperature, which is affine).
struct Unit {
    aliases: &'static [&'static str],
    symbol: &'static str,
    category: Category,
    factor: f64,
}

const UNITS: &[Unit] = &[
    // Length, in metres.
    Unit {
        aliases: &[
            "mm",
            "millimeter",
            "millimeters",
            "millimetre",
            "millimetres",
        ],
        symbol: "mm",
        category: Category::Length,
        factor: 0.001,
    },
    Unit {
        aliases: &[
            "cm",
            "centimeter",
            "centimeters",
            "centimetre",
            "centimetres",
        ],
        symbol: "cm",
        category: Category::Length,
        factor: 0.01,
    },
    Unit {
        aliases: &["m", "meter", "meters", "metre", "metres"],
        symbol: "m",
        category: Category::Length,
        factor: 1.0,
    },
    Unit {
        aliases: &["km", "kilometer", "kilometers", "kilometre", "kilometres"],
        symbol: "km",
        category: Category::Length,
        factor: 1000.0,
    },
    Unit {
        aliases: &["in", "inch", "inches"],
        symbol: "in",
        category: Category::Length,
        factor: 0.0254,
    },
    Unit {
        aliases: &["ft", "foot", "feet"],
        symbol: "ft",
        category: Category::Length,
        factor: 0.3048,
    },
    Unit {
        aliases: &["yd", "yard", "yards"],
        symbol: "yd",
        category: Category::Length,
        factor: 0.9144,
    },
    Unit {
        aliases: &["mi", "mile", "miles"],
        symbol: "mi",
        category: Category::Length,
        factor: 1609.344,
    },
    // Mass, in grams.
    Unit {
        aliases: &["mg", "milligram", "milligrams"],
        symbol: "mg",
        category: Category::Mass,
        factor: 0.001,
    },
    Unit {
        aliases: &["g", "gram", "grams", "gramme", "grammes"],
        symbol: "g",
        category: Category::Mass,
        factor: 1.0,
    },
    Unit {
        aliases: &["kg", "kilogram", "kilograms", "kilo", "kilos"],
        symbol: "kg",
        category: Category::Mass,
        factor: 1000.0,
    },
    Unit {
        aliases: &["t", "tonne", "tonnes"],
        symbol: "t",
        category: Category::Mass,
        factor: 1_000_000.0,
    },
    Unit {
        aliases: &["lb", "lbs", "pound", "pounds"],
        symbol: "lb",
        category: Category::Mass,
        factor: 453.59237,
    },
    Unit {
        aliases: &["oz", "ounce", "ounces"],
        symbol: "oz",
        category: Category::Mass,
        factor: 28.349_523_125,
    },
    Unit {
        aliases: &["st", "stone", "stones"],
        symbol: "st",
        category: Category::Mass,
        factor: 6_350.293_18,
    },
    // Temperature, affine around Celsius.
    Unit {
        aliases: &["c", "celsius", "centigrade", "°c"],
        symbol: "°C",
        category: Category::Temperature,
        factor: 1.0,
    },
    Unit {
        aliases: &["f", "fahrenheit", "°f"],
        symbol: "°F",
        category: Category::Temperature,
        factor: 1.0,
    },
    Unit {
        aliases: &["k", "kelvin", "°k"],
        symbol: "K",
        category: Category::Temperature,
        factor: 1.0,
    },
    // Volume, in litres (US liquid measures).
    Unit {
        aliases: &[
            "ml",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitres",
        ],
        symbol: "mL",
        category: Category::Volume,
        factor: 0.001,
    },
    Unit {
        aliases: &[
            "cl",
            "centiliter",
            "centiliters",
            "centilitre",
            "centilitres",
        ],
        symbol: "cL",
        category: Category::Volume,
        factor: 0.01,
    },
    Unit {
        aliases: &["dl", "deciliter", "deciliters", "decilitre", "decilitres"],
        symbol: "dL",
        category: Category::Volume,
        factor: 0.1,
    },
    Unit {
        aliases: &["l", "liter", "liters", "litre", "litres"],
        symbol: "L",
        category: Category::Volume,
        factor: 1.0,
    },
    Unit {
        aliases: &["gal", "gallon", "gallons"],
        symbol: "gal",
        category: Category::Volume,
        factor: 3.785_411_784,
    },
    Unit {
        aliases: &["qt", "quart", "quarts"],
        symbol: "qt",
        category: Category::Volume,
        factor: 0.946_352_946,
    },
    Unit {
        aliases: &["pt", "pint", "pints"],
        symbol: "pt",
        category: Category::Volume,
        factor: 0.473_176_473,
    },
    Unit {
        aliases: &["cup", "cups"],
        symbol: "cup",
        category: Category::Volume,
        factor: 0.236_588_236_5,
    },
    Unit {
        aliases: &["floz", "fl-oz", "fluidounce", "fluidounces"],
        symbol: "fl oz",
        category: Category::Volume,
        factor: 0.029_573_529_562_5,
    },
    Unit {
        aliases: &["tbsp", "tablespoon", "tablespoons"],
        symbol: "tbsp",
        category: Category::Volume,
        factor: 0.014_786_764_781_25,
    },
    Unit {
        aliases: &["tsp", "teaspoon", "teaspoons"],
        symbol: "tsp",
        category: Category::Volume,
        factor: 0.004_928_921_593_75,
    },
];

/// The words that may join the two units.
fn is_joiner(word: &str) -> bool {
    matches!(word, "in" | "to")
}

/// The unit a word names, if it names one.
fn lookup(word: &str) -> Option<&'static Unit> {
    UNITS.iter().find(|unit| unit.aliases.contains(&word))
}

/// Read the expression as a conversion, or `None` when it is not one.
pub(super) fn convert(expression: &str) -> Option<Evaluation> {
    let mut words = expression.split_whitespace();
    let (value, first_unit) = number_and_unit(words.next()?)?;

    let mut units: Vec<&str> = Vec::new();
    if let Some(glued) = first_unit {
        units.push(glued);
    }
    units.extend(words);

    let (from, to) = targets(&units)?;
    let value = convert_value(value, from, to)?;
    if !value.is_finite() {
        return None;
    }

    Some(Evaluation {
        expression: expression.to_string(),
        value,
        unit: to.symbol.to_string(),
    })
}

/// Split `10km` into `10` and `km`; a bare number yields no unit.
fn number_and_unit(token: &str) -> Option<(f64, Option<&str>)> {
    let chars: Vec<char> = token.chars().collect();
    let (value, consumed) = expr::number(&chars, 0)?;
    let unit = token
        .char_indices()
        .nth(consumed)
        .map(|(at, _)| &token[at..]);
    Some((value, unit))
}

/// The two units of a conversion: `<unit> [<joiner>] <unit>`.
fn targets(units: &[&str]) -> Option<(&'static Unit, &'static Unit)> {
    let (from, to) = match units {
        [from, to] if !is_joiner(to) => (*from, *to),
        [from, joiner, to] if is_joiner(joiner) => (*from, *to),
        _ => return None,
    };

    let from = lookup(from)?;
    let to = lookup(to)?;
    (from.category == to.category).then_some((from, to))
}

/// Move a value between two units of one category.
fn convert_value(value: f64, from: &Unit, to: &Unit) -> Option<f64> {
    if from.category == Category::Temperature {
        let celsius = match from.symbol {
            "°C" => value,
            "°F" => (value - 32.0) * 5.0 / 9.0,
            "K" => value - 273.15,
            _ => return None,
        };
        return Some(match to.symbol {
            "°C" => celsius,
            "°F" => celsius * 9.0 / 5.0 + 32.0,
            "K" => celsius + 273.15,
            _ => return None,
        });
    }

    Some(value * from.factor / to.factor)
}

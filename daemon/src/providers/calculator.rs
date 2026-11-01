//! The `calculator` provider: when the query is an expression, exactly one candidate.
//!
//! The row is a Top-Hit-only candidate (contract §5): its base weight of `0.40` and the fact that
//! it is the only row a parse produces are what put it on top. A query that does not parse
//! contributes no row at all — never an error row (story 28).

use crate::calc::{self, Evaluation};
use crate::protocol::{Action, ResultItem, Row};
use crate::providers::{Provider, QueryRequest};

/// Reads the query as an expression.
pub struct Calculator;

impl Calculator {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Calculator {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for Calculator {
    fn id(&self) -> &'static str {
        "calculator"
    }

    fn available(&self) -> bool {
        true
    }

    fn query(&self, request: &QueryRequest) -> Vec<ResultItem> {
        calc::evaluate(&request.text)
            .map(|evaluation| vec![candidate(&evaluation)])
            .unwrap_or_default()
    }
}

/// The single result a recognised expression produces: contract §3 row, §6 preview, and the
/// `copy` action that Enter runs.
pub fn candidate(evaluation: &Evaluation) -> ResultItem {
    let mut item = ResultItem::new(
        "calculator",
        format!("calc:{}", evaluation.expression),
        evaluation.display(),
    );
    item.subtitle = evaluation.expression.clone();
    item.actions = vec![Action::new("copy", "Copy", "copy")];
    item.preview = Row::from([
        (
            "kind".to_string(),
            zbus::Variant::new("calculator".to_string()),
        ),
        (
            "expression".to_string(),
            zbus::Variant::new(evaluation.expression.clone()),
        ),
        (
            "value".to_string(),
            zbus::Variant::new(evaluation.value_text()),
        ),
        (
            "unit".to_string(),
            zbus::Variant::new(evaluation.unit.clone()),
        ),
    ]);
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use zbus::{DynamicType, OwnedVariant};

    use crate::protocol::string_field;

    fn query(text: &str) -> Vec<ResultItem> {
        Calculator::new().query(&QueryRequest::new(text, Vec::new()))
    }

    fn only(text: &str) -> ResultItem {
        let mut results = query(text);
        assert_eq!(results.len(), 1, "`{text}` must produce exactly one row");
        results.remove(0)
    }

    #[test]
    fn the_provider_is_the_contract_calculator() {
        let calculator = Calculator::new();
        assert_eq!(calculator.id(), "calculator");
        assert!(calculator.available());
    }

    #[test]
    fn an_expression_is_one_copy_candidate_with_the_contract_preview() {
        let item = only("2 + 2");

        assert_eq!(item.provider, "calculator");
        assert_eq!(
            item.id, "calc:2 + 2",
            "the id carries the normalised expression"
        );
        assert_eq!(item.title, "4");
        assert_eq!(item.subtitle, "2 + 2");
        assert_eq!(item.icon, "", "the calculator has no themed icon");
        assert_eq!(item.score, 0.0, "the registry scores it, not the provider");
        assert_eq!(item.actions, vec![Action::new("copy", "Copy", "copy")]);

        assert_eq!(DynamicType::signature(&item.preview).to_string(), "a{sv}");
        assert_eq!(
            string_field(&item.preview, "kind").as_deref(),
            Some("calculator")
        );
        assert_eq!(
            string_field(&item.preview, "expression").as_deref(),
            Some("2 + 2")
        );
        assert_eq!(string_field(&item.preview, "value").as_deref(), Some("4"));
        assert_eq!(string_field(&item.preview, "unit").as_deref(), Some(""));
    }

    #[test]
    fn a_conversion_carries_the_target_unit() {
        let item = only("10 km in miles");

        assert_eq!(item.id, "calc:10 km in miles");
        assert_eq!(item.title, "6.213712 mi");
        assert_eq!(
            string_field(&item.preview, "value").as_deref(),
            Some("6.213712")
        );
        assert_eq!(string_field(&item.preview, "unit").as_deref(), Some("mi"));
    }

    #[test]
    fn the_row_survives_the_wire() {
        let row = only("100 c in f").row();
        let preview =
            HashMap::<String, OwnedVariant>::try_from(row["preview"].try_clone().unwrap())
                .expect("preview must be a{sv}");
        let preview = crate::protocol::owned_row(preview);
        assert_eq!(string_field(&preview, "value").as_deref(), Some("212"));
        assert_eq!(string_field(&preview, "unit").as_deref(), Some("°F"));

        let round_tripped = ResultItem::from_row(&row).expect("the row parses back");
        assert_eq!(round_tripped.id, "calc:100 c in f");
        assert_eq!(round_tripped.title, "212 °F");
        assert_eq!(
            round_tripped.actions,
            vec![Action::new("copy", "Copy", "copy")]
        );
        assert_eq!(
            string_field(&round_tripped.preview, "expression").as_deref(),
            Some("100 c in f")
        );
    }

    #[test]
    fn text_that_is_not_an_expression_contributes_nothing() {
        for text in [
            "", "   ", "nautilus", "abc 123", "2 +", "10 km", "km", "1/0",
        ] {
            assert!(
                query(text).is_empty(),
                "`{text}` must produce no row at all"
            );
        }
    }

    #[test]
    fn every_preview_field_is_a_string() {
        let item = only("50%");
        for key in ["kind", "expression", "value", "unit"] {
            assert!(
                item.preview.contains_key(key),
                "the preview must carry `{key}`"
            );
            assert!(
                string_field(&item.preview, key).is_some(),
                "`{key}` must be a string"
            );
        }
        assert_eq!(string_field(&item.preview, "value").as_deref(), Some("0.5"));
    }
}

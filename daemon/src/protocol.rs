//! Identity constants, the eight provider ids and the on-the-wire result schema.
//!
//! This module is the Rust half of the shared contract (v1). The GJS half lives in
//! `extension/lib/protocol.js`; the two must stay in step.

use std::collections::HashMap;

use zbus::Variant;

/// The well-known name whose ownership is Lumen's single-instance lock.
pub const BUS_NAME: &str = "org.gnome.Lumen";
/// The object path the engine lives at.
pub const OBJECT_PATH: &str = "/org/gnome/Lumen";
/// The engine interface.
pub const INTERFACE: &str = "org.gnome.Lumen.Engine";

/// Protocol major version, installed in lockstep with the extension.
pub const PROTOCOL_MAJOR: u32 = 1;

/// GNOME Shell extension uuid.
pub const EXTENSION_UUID: &str = "lumen@kethan.dev";
/// GSettings schema id, shared by both processes.
pub const SCHEMA_ID: &str = "org.gnome.shell.extensions.lumen";
/// GSettings path of that schema.
pub const SCHEMA_PATH: &str = "/org/gnome/shell/extensions/lumen/";

/// Every provider id, in the order used when nothing else decides.
pub const PROVIDER_IDS: [&str; 8] = [
    "applications",
    "files",
    "recent",
    "clipboard",
    "calculator",
    "actions",
    "settings",
    "websearch",
];

/// The ids that may appear in the `Capabilities` property, in report order.
pub const CAPABILITY_IDS: [&str; 5] = ["files", "recent", "clipboard", "calculator", "websearch"];

/// Shell-only actions the daemon routes back to the extension instead of performing.
pub const SHELL_ACTIONS: [&str; 1] = ["open-url"];

/// The only values an action's `handler` field may take.
pub const ACTION_HANDLERS: [&str; 4] = ["open", "copy", "reveal", "shell"];

/// The fields every result row carries, and their `a{sv}` signature.
pub const RESULT_FIELDS: [&str; 8] = [
    "id", "provider", "title", "subtitle", "icon", "score", "actions", "preview",
];

/// `a{sv}`: a result row, or any provider-specific payload.
pub type Row = HashMap<String, Variant<'static>>;
/// `aa{sv}`: the `results` argument of `ResultsChanged`.
pub type Rows = Vec<Row>;

/// The id prefix that identifies the provider owning a result, from contract §3.
pub fn id_prefix(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "applications" => "app:",
        "files" => "file:",
        "recent" => "recent:",
        "clipboard" => "clip:",
        "calculator" => "calc:",
        "actions" => "action:",
        "settings" => "setting:",
        "websearch" => "web:",
        _ => return None,
    })
}

/// One entry of a result's `actions` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub handler: String,
}

impl Action {
    pub fn new(id: impl Into<String>, label: impl Into<String>, handler: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            handler: handler.into(),
        }
    }

    /// `a{sv}` with the three fields of contract §3.
    pub fn row(&self) -> Row {
        Row::from([
            ("id".to_string(), Variant::new(self.id.clone())),
            ("label".to_string(), Variant::new(self.label.clone())),
            ("handler".to_string(), Variant::new(self.handler.clone())),
        ])
    }
}

/// A single result, in the schema shared with the extension.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultItem {
    pub id: String,
    pub provider: String,
    pub title: String,
    pub subtitle: String,
    pub icon: String,
    pub score: f64,
    pub actions: Vec<Action>,
    pub preview: Row,
}

impl ResultItem {
    pub fn new(provider: impl Into<String>, id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            title: title.into(),
            subtitle: String::new(),
            icon: String::new(),
            score: 0.0,
            actions: Vec::new(),
            preview: Row::new(),
        }
    }

    /// The `a{sv}` form that goes on the wire and into the extension's model.
    pub fn row(&self) -> Row {
        let actions: Vec<Row> = self.actions.iter().map(Action::row).collect();
        Row::from([
            ("id".to_string(), Variant::new(self.id.clone())),
            ("provider".to_string(), Variant::new(self.provider.clone())),
            ("title".to_string(), Variant::new(self.title.clone())),
            ("subtitle".to_string(), Variant::new(self.subtitle.clone())),
            ("icon".to_string(), Variant::new(self.icon.clone())),
            ("score".to_string(), Variant::new(self.score)),
            ("actions".to_string(), Variant::new(actions)),
            ("preview".to_string(), Variant::new(self.preview.clone())),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::DynamicType;

    #[test]
    fn result_row_is_the_contract_schema() {
        let mut item = ResultItem::new("applications", "app:org.gnome.Nautilus.desktop", "Files");
        item.subtitle = "Browse your files".to_string();
        item.icon = "org.gnome.Nautilus".to_string();
        item.score = 1.3;
        item.actions = vec![Action::new("open", "Open", "open")];

        let row = item.row();

        assert_eq!(DynamicType::signature(&row).to_string(), "a{sv}");

        let mut keys: Vec<&String> = row.keys().collect();
        keys.sort();
        let mut expected: Vec<&str> = RESULT_FIELDS.to_vec();
        expected.sort();
        assert_eq!(keys, expected);

        assert_eq!(String::try_from(&row["title"]).unwrap(), "Files");
        assert_eq!(f64::try_from(&row["score"]).unwrap(), 1.3);
        assert_eq!(String::try_from(&row["provider"]).unwrap(), "applications");

        let actions = Vec::<HashMap<String, zbus::OwnedVariant>>::try_from(
            row["actions"].try_clone().unwrap(),
        )
        .expect("actions must be aa{sv}");
        assert_eq!(actions.len(), 1);
        let handler: &str = (&actions[0]["handler"]).try_into().unwrap();
        assert_eq!(handler, "open");
        let label: &str = (&actions[0]["label"]).try_into().unwrap();
        assert_eq!(label, "Open");

        let preview = HashMap::<String, zbus::OwnedVariant>::try_from(
            row["preview"].try_clone().unwrap(),
        )
        .expect("preview must be a{sv}, and is present even when empty");
        assert!(preview.is_empty());
    }

    #[test]
    fn every_provider_has_an_id_prefix() {
        for provider in PROVIDER_IDS {
            assert!(
                id_prefix(provider).is_some(),
                "{provider} has no id prefix"
            );
        }
        assert_eq!(id_prefix("nonsense"), None);
        assert_eq!(id_prefix("websearch"), Some("web:"));
    }

    #[test]
    fn capabilities_are_drawn_from_the_contract_list() {
        for id in CAPABILITY_IDS {
            assert!(PROVIDER_IDS.contains(&id), "{id} is not a provider");
        }
    }
}

//! The daemon's D-Bus seam: the real binary, a real session bus, a real client.

mod support;

use std::collections::HashMap;

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, injected, injected_options, name_is_owned, Harness};
use zbus::OwnedVariant;

/// `a{sv}`, the shape of `Query`'s `options` argument.
type Options = HashMap<String, zbus::OwnedVariant>;

#[test]
fn daemon_owns_the_lumen_name_and_reports_a_working_engine() {
    let mut harness = Harness::spawn_with_default_name();
    let address = harness.address().to_string();
    let name = harness.name().to_string();
    let bus_name = name.clone();

    let (owned, version, capabilities, clipboard_enabled) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let owned = name_is_owned(&connection, &bus_name)
            .await
            .expect("NameHasOwner");
        let proxy = engine_proxy(&connection, &bus_name)
            .await
            .expect("engine proxy");

        let version: u32 = proxy.get_property("Version").await.expect("Version");
        let capabilities: Vec<String> =
            proxy.get_property("Capabilities").await.expect("Capabilities");
        let clipboard_enabled: bool = proxy
            .get_property("ClipboardEnabled")
            .await
            .expect("ClipboardEnabled");

        (owned, version, capabilities, clipboard_enabled)
    });

    assert!(owned, "the daemon must own {name}");
    assert_eq!(version, 1, "protocol major version");
    assert_eq!(
        capabilities,
        vec![
            "recent".to_string(),
            "clipboard".to_string(),
            "calculator".to_string(),
            "websearch".to_string()
        ],
        "without an indexer the files capability is absent; recent documents and the clipboard are always there"
    );
    assert!(!clipboard_enabled, "clipboard history is off by default");

    let status = harness.terminate();
    assert!(status.success(), "SIGTERM must be a clean exit: {status}");
}

#[test]
fn daemon_owns_a_unique_name_when_asked_to() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();
    assert_ne!(name, "org.gnome.Lumen");
    let bus_name = name.clone();

    let owned = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        name_is_owned(&connection, &bus_name)
            .await
            .expect("NameHasOwner")
    });

    assert!(owned, "the daemon must own {name}");
}

#[test]
fn query_pushes_results_changed_then_search_finished() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (search_id, changed_id, results, finished_id, after_finish) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");

        let mut changed = proxy
            .receive_signal("ResultsChanged")
            .await
            .expect("ResultsChanged stream");
        let mut finished = proxy
            .receive_signal("SearchFinished")
            .await
            .expect("SearchFinished stream");

        let search_id: String = proxy
            .call("Query", &("nautilus", vec!["applications"], Options::new()))
            .await
            .expect("Query");
        assert!(!search_id.is_empty(), "Query must return a search id");

        let changed = changed.next().await.expect("ResultsChanged signal");
        let (changed_id, results): (String, Vec<HashMap<String, zbus::OwnedVariant>>) = changed
            .body()
            .deserialize()
            .expect("ResultsChanged body");

        let finished = finished.next().await.expect("SearchFinished signal");
        let finished_id: String = finished
            .body()
            .deserialize()
            .expect("SearchFinished body");

        let owned = name_is_owned(&connection, &name).await.expect("NameHasOwner");
        (search_id, changed_id, results, finished_id, owned)
    });

    assert_eq!(changed_id, search_id, "the signals carry the returned id");
    assert!(results.is_empty(), "the provider stubs contribute nothing yet");
    assert_eq!(finished_id, search_id, "the signals carry the returned id");
    assert!(after_finish, "the daemon stays on the bus after a search");
}

#[test]
fn two_sequential_queries_each_finish() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");

        for text in ["first", "second"] {
            let mut finished = proxy
                .receive_signal("SearchFinished")
                .await
                .expect("SearchFinished stream");
            let id: String = proxy
                .call("Query", &(text, Vec::<String>::new(), Options::new()))
                .await
                .expect("Query");
            let signal = finished.next().await.expect("SearchFinished signal");
            let finished_id: String = signal.body().deserialize().expect("SearchFinished body");
            assert_eq!(finished_id, id, "query {text} must report its own id");
        }
    });
}

#[test]
fn second_daemon_loses_the_name_race_and_exits() {
    let mut first = Harness::spawn_with_default_name();
    let address = first.address().to_string();

    let mut second = std::process::Command::new(env!("CARGO_BIN_EXE_lumen-daemon"))
        .env("DBUS_SESSION_BUS_ADDRESS", &address)
        // This test is about the name, not the store: keep it off the disk and off the keyring.
        .args(["--clipboard", "memory"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn a second daemon");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        match second.try_wait().expect("wait for the second daemon") {
            Some(status) => break Some(status),
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            None => break None,
        }
    };

    let status = match status {
        Some(status) => status,
        None => {
            let _ = second.kill();
            let _ = second.wait();
            panic!("a daemon that cannot take the name must exit rather than run without it");
        }
    };
    assert!(
        !status.success(),
        "a daemon that cannot take the name must fail: {status}"
    );

    assert!(first.terminate().success());
}

fn text_of(row: &HashMap<String, zbus::OwnedVariant>, key: &str) -> String {
    String::try_from(row[key].try_clone().expect("cloneable")).expect("a string")
}

fn number_of(row: &HashMap<String, OwnedVariant>, key: &str) -> f64 {
    f64::try_from(row[key].try_clone().expect("cloneable")).expect("a number")
}

fn preview_of(row: &HashMap<String, OwnedVariant>) -> HashMap<String, OwnedVariant> {
    HashMap::try_from(row["preview"].clone()).expect("preview must be a{sv}")
}

fn actions_of(row: &HashMap<String, OwnedVariant>) -> Vec<HashMap<String, OwnedVariant>> {
    Vec::try_from(row["actions"].clone()).expect("actions must be aa{sv}")
}

/// One `Query` round trip: the rows the matching `ResultsChanged` carried.
async fn search(
    proxy: &zbus::Proxy<'_>,
    text: &str,
    enabled: Vec<&str>,
    options: Options,
) -> Vec<HashMap<String, OwnedVariant>> {
    let mut changed = proxy
        .receive_signal("ResultsChanged")
        .await
        .expect("ResultsChanged stream");
    let _: String = proxy
        .call("Query", &(text, enabled, options))
        .await
        .expect("Query");
    let signal = changed.next().await.expect("ResultsChanged signal");
    let (_id, results): (String, Vec<HashMap<String, OwnedVariant>>) =
        signal.body().deserialize().expect("ResultsChanged body");
    results
}

#[test]
fn a_valid_expression_is_the_top_hit_with_its_value_and_a_copy_action() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let results = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let options = injected_options(vec![injected(
            "app:org.gnome.Nautilus.desktop",
            "applications",
            "Files",
        )]);
        search(&proxy, "2 + 2", vec!["applications", "calculator"], options).await
    });

    let calculators = results
        .iter()
        .filter(|row| text_of(row, "provider") == "calculator")
        .count();
    assert_eq!(
        calculators, 1,
        "a parsed expression yields exactly one calculator row"
    );
    assert_eq!(
        text_of(&results[0], "provider"),
        "calculator",
        "and it is the Top Hit, ahead of the injected application"
    );
    assert_eq!(text_of(&results[0], "id"), "calc:2 + 2");
    assert_eq!(text_of(&results[0], "title"), "4");
    assert!(
        (number_of(&results[0], "score") - 0.40).abs() < 1e-9,
        "the calculator's base weight is 0.40, got {}",
        number_of(&results[0], "score")
    );

    let preview = preview_of(&results[0]);
    assert_eq!(text_of(&preview, "kind"), "calculator");
    assert_eq!(text_of(&preview, "expression"), "2 + 2");
    assert_eq!(text_of(&preview, "value"), "4");
    assert_eq!(text_of(&preview, "unit"), "", "arithmetic has no unit");

    let actions = actions_of(&results[0]);
    assert_eq!(actions.len(), 1, "the calculator offers one action");
    assert_eq!(text_of(&actions[0], "id"), "copy");
    assert_eq!(text_of(&actions[0], "handler"), "copy");
}

#[test]
fn a_unit_conversion_is_the_top_hit_with_the_converted_value() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let results = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        search(&proxy, "10 km in miles", vec!["calculator"], Options::new()).await
    });

    assert_eq!(results.len(), 1, "one conversion, one row");
    assert_eq!(text_of(&results[0], "id"), "calc:10 km in miles");
    assert_eq!(text_of(&results[0], "title"), "6.213712 mi");
    let preview = preview_of(&results[0]);
    assert_eq!(text_of(&preview, "kind"), "calculator");
    assert_eq!(text_of(&preview, "value"), "6.213712");
    assert_eq!(text_of(&preview, "unit"), "mi");
}

#[test]
fn malformed_input_yields_no_result_at_all() {
    const MALFORMED: [&str; 4] = ["2 +", "(2 + 3", "10 km in bananas", "abc 123"];

    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let mut rows = Vec::new();
        for text in MALFORMED {
            rows.push(search(&proxy, text, vec!["calculator"], Options::new()).await);
        }
        rows
    });

    for (text, results) in MALFORMED.iter().zip(rows) {
        assert!(
            results.is_empty(),
            "`{text}` must be ignored, never shown as an error row"
        );
    }
}

#[test]
fn an_ordinary_word_query_never_reaches_the_calculator() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let results = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        search(
            &proxy,
            "nautilus",
            vec!["applications", "calculator"],
            Options::new(),
        )
        .await
    });

    assert!(results.is_empty(), "a word is a search, not an expression");
}

#[test]
fn injected_candidates_are_ranked_and_an_exact_application_is_the_top_hit() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (providers, titles, scores) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let mut changed = proxy
            .receive_signal("ResultsChanged")
            .await
            .expect("ResultsChanged stream");

        let options = injected_options(vec![
            injected("file:/home/me/Files", "files", "Files"),
            injected("app:org.gnome.Nautilus.desktop", "applications", "Files"),
        ]);
        let _: String = proxy
            .call(
                "Query",
                &("files", vec!["applications", "files"], options),
            )
            .await
            .expect("Query");

        let signal = changed.next().await.expect("ResultsChanged signal");
        let (_id, results): (String, Vec<HashMap<String, OwnedVariant>>) =
            signal.body().deserialize().expect("ResultsChanged body");

        let providers: Vec<String> = results.iter().map(|r| text_of(r, "provider")).collect();
        let titles: Vec<String> = results.iter().map(|r| text_of(r, "title")).collect();
        let scores: Vec<f64> = results.iter().map(|r| number_of(r, "score")).collect();
        (providers, titles, scores)
    });

    assert_eq!(
        providers,
        vec!["applications".to_string(), "files".to_string()],
        "the exact application match must outrank the file"
    );
    assert_eq!(titles, vec!["Files".to_string(), "Files".to_string()]);
    assert!(
        (scores[0] - 1.30).abs() < 1e-9,
        "an exact app match scores base 0.30 + quality 1.0, got {}",
        scores[0]
    );
    assert!(scores[0] > scores[1]);
}

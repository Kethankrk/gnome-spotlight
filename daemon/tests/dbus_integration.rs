//! The daemon's D-Bus seam: the real binary, a real session bus, a real client.

mod support;

use std::collections::HashMap;

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, name_is_owned, Harness};

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
        vec!["calculator".to_string(), "websearch".to_string()],
        "without an indexer the files capability is absent"
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

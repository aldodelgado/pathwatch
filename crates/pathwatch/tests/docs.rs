//! The documentation in `docs/`, held against the program it describes.
//!
//! Two of the documents are what the program prints, so they are compared
//! whole; `UPDATE_DOCS=1` rewrites them. The configuration reference is
//! written by hand and checked setting by setting. The capability guide is
//! checked against what `doctor` reports on the host running the tests.
//! Nothing here sends anything.

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};

use pathwatch_core::config::{NO_EFFECT, reloadable};

fn docs_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

fn doc(name: &str) -> String {
    let path = docs_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Runs the program with no configuration file of the user's in reach.
fn pathwatch(args: &[&str]) -> String {
    let empty = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("docs-empty-config.toml");
    std::fs::write(&empty, "").expect("scratch file");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pathwatch"));
    cmd.args(args).stdin(Stdio::null()).env("PATHWATCH_CONFIG", &empty);
    for name in ["PATHWATCH_NEXTDNS_PROFILE", "NO_COLOR"] {
        cmd.env_remove(name);
    }
    let out = cmd.output().expect("run pathwatch");
    assert!(out.status.success(), "pathwatch {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("UTF-8 output")
}

/// A document that is the program's own output.
fn printed(name: &str, args: &[&str]) {
    let now = pathwatch(args);
    if std::env::var_os("UPDATE_DOCS").is_some() {
        std::fs::write(docs_dir().join(name), &now).expect("write the document");
    }
    assert!(doc(name) == now, "docs/{name} is not what `pathwatch {}` prints; rerun the tests with UPDATE_DOCS=1 and review the change", args.join(" "));
}

#[test]
fn the_manual_page_is_the_one_the_program_prints() {
    printed("pathwatch.1", &["man"]);
    let page = doc("pathwatch.1");
    // Every command and every long option the program has is in it.
    for command in ["run", "demo", "sample", "replay", "export", "doctor", "config check", "config show", "config path", "config example", "config reference", "glossary", "man"] {
        assert!(page.contains(&format!(".SS pathwatch {command}\n")), "no section for `{command}`");
    }
    for option in ["config", "assert", "format", "record", "theme", "reduced\\-motion", "mouse", "ascii", "color", "probe", "json", "markdown", "redact\\-local", "speed"] {
        assert!(page.contains(&format!("\\fB\\-\\-{option}\\fR")), "no entry for --{option}");
    }
    assert!(!page.contains("panic"), "options that exist only for the tests are not documented");
    // Every exit status the program can end with.
    for code in ["0", "1", "2", "3", "4", "5", "10", "130, 143"] {
        assert!(page.contains(&format!(".TP\n.B {code}\n")), "exit status {code}");
    }
}

#[test]
fn the_glossary_is_the_one_the_program_prints() {
    printed("glossary.md", &["glossary", "--markdown"]);
}

/// `| `key` | `default` | meaning | reload |` rows, by key.
fn reference_rows(text: &str) -> BTreeMap<String, (String, String, String)> {
    let mut rows = BTreeMap::new();
    for line in text.lines().filter(|l| l.starts_with("| `")) {
        let cells: Vec<&str> = line.trim_matches('|').split(" | ").map(str::trim).collect();
        // The environment table has two columns and is not about settings.
        if cells.len() < 4 {
            continue;
        }
        let [key, default, .., last] = cells.as_slice() else { panic!("not a row: {line}") };
        let meaning = cells[2..cells.len() - 1].join(" | ");
        let key = key.trim_matches('`').to_string();
        assert!(rows.insert(key.clone(), (default.to_string(), meaning, last.to_string())).is_none(), "{key} is listed twice");
    }
    rows
}

/// A default as the reference writes it.
fn shown(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Array(items) => format!("`[{}]`", items.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")),
        other => format!("`{other}`"),
    }
}

fn flatten(prefix: &str, v: &serde_json::Value, out: &mut BTreeMap<String, serde_json::Value>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                // Tables whose keys are the user's own, and the list of queries, are described by shape.
                if key == "providers" || key == "ui.keys" || key == "dns.query" {
                    continue;
                }
                flatten(&key, child, out);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.clone());
        }
    }
}

#[test]
fn the_configuration_reference_is_printed_by_the_program_and_covers_every_setting() {
    let reference = doc("configuration.md");
    assert!(pathwatch(&["config", "reference"]) == reference, "`pathwatch config reference` does not print docs/configuration.md");
    let rows = reference_rows(&reference);

    // Every setting the program has, with its default, is a row; and the other way round.
    let effective: serde_json::Value = serde_json::from_str(&pathwatch(&["config", "show", "--format", "json"])).expect("the effective configuration as JSON");
    let mut settings = BTreeMap::new();
    flatten("", &effective, &mut settings);
    for (key, value) in &settings {
        let (default, _, _) = rows.get(key).unwrap_or_else(|| panic!("{key} is not in the reference"));
        assert_eq!(default, &shown(value), "the default of {key}");
    }
    // Providers, targets and queries: every field one can have, taken from the built-in ones.
    let mut shaped = BTreeSet::from(["ui.keys".to_string()]);
    for provider in effective["providers"].as_object().expect("providers").values() {
        for (field, value) in provider.as_object().expect("a provider") {
            match value.as_array().filter(|_| field == "targets") {
                Some(targets) => shaped.extend(targets.iter().flat_map(|t| t.as_object().expect("a target").keys()).map(|f| format!("providers.<key>.targets[].{f}"))),
                None => {
                    shaped.insert(format!("providers.<key>.{field}"));
                }
            }
        }
    }
    shaped.extend(effective["dns"]["query"][0].as_object().expect("a query").keys().map(|f| format!("dns.query[].{f}")));
    // Fields no built-in target happens to set.
    shaped.extend(["role", "port", "addresses", "follow", "enabled"].map(|f| format!("providers.<key>.targets[].{f}")));
    for key in &shaped {
        assert!(rows.contains_key(key), "{key} is not in the reference");
    }
    let known: BTreeSet<&String> = settings.keys().chain(&shaped).collect();
    for key in rows.keys() {
        assert!(known.contains(key), "the reference lists {key}, which the program does not have");
    }

    // What a reload applies, and what changes nothing, are said of exactly the right settings.
    for (key, (_, meaning, reload)) in &rows {
        let live = reloadable(&key.replace(".<key>", ".x").replace("[]", ""));
        assert_eq!(reload, if live { "yes" } else { "no" }, "the reload column of {key}");
        let inert = NO_EFFECT.contains(&key.as_str()) || key == "providers.<key>.color";
        assert_eq!(meaning.contains("No effect in this build"), inert, "{key}: {meaning}");
    }
    for key in NO_EFFECT {
        assert!(rows.contains_key(key), "{key} has no effect and is not in the reference");
    }
}

/// The commented example is the first thing a user reads about a setting:
/// where one changes nothing, the comment beside it, or over its table,
/// says so.
#[test]
fn the_example_file_says_which_settings_change_nothing() {
    let example = pathwatch(&["config", "example"]);
    const PHRASE: &str = "no effect in this build";
    let (mut section, mut section_inert, mut pending) = (String::new(), false, false);
    let mut said = BTreeSet::new();
    for line in example.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            pending |= line.contains(PHRASE);
        } else if line.starts_with('[') {
            section = line.trim_start_matches('[').split(']').next().unwrap_or("").to_string();
            section_inert = pending || line.contains(PHRASE);
            pending = false;
            if section_inert {
                said.insert(section.clone());
            }
        } else if let Some((key, rest)) = line.split_once('=') {
            if section_inert || rest.contains(PHRASE) {
                said.insert(format!("{section}.{}", key.trim()));
            }
            pending = false;
        } else if line.is_empty() {
            pending = false;
        }
    }
    for key in NO_EFFECT {
        assert!(said.contains(key), "the example does not say that {key} has no effect");
    }
    // And of nothing that does something.
    let inert: BTreeSet<&str> = NO_EFFECT.into_iter().collect();
    for key in &said {
        assert!(
            inert.contains(key.as_str()) || inert.iter().any(|k| k.strip_prefix(key.as_str()).is_some_and(|rest| rest.starts_with('.'))) || key.starts_with("ui.keys."),
            "{key} is said to have no effect, and has one"
        );
    }
    assert!(example.starts_with("# pathwatch configuration: the built-in defaults, as a commented file.\n"));
    assert!(example.contains("`color` has no effect"));
}

#[test]
fn the_capability_guide_agrees_with_doctor_on_this_host() {
    let guide = doc("capabilities.md");
    let report: serde_json::Value = serde_json::from_str(&pathwatch(&["doctor", "--json"])).expect("doctor's report as JSON");
    let said = pathwatch(&["doctor"]);

    // Every line doctor prints on this host is one the guide explains.
    let table: String = guide.lines().filter(|l| l.starts_with("| `")).collect::<Vec<_>>().join("\n");
    for line in said.lines().filter(|l| l.len() > 16 && !l.starts_with(' ') && l.as_bytes()[15] == b' ' && !l.starts_with("Nothing was sent")) {
        let label = line[..16].trim();
        assert!(table.contains(&format!("`{label}`")), "the guide does not explain doctor's line {label:?}");
    }

    // The rule the guide gives for ICMP is the one this host follows.
    let groups: Vec<u64> = report["group_ids"].as_array().expect("group ids").iter().filter_map(serde_json::Value::as_u64).collect();
    let available = |family: &str| report["icmp"].as_array().expect("icmp").iter().any(|c| c["family"] == family && c["available"] == true);
    match report["os"].as_str() {
        Some("macos") => {
            assert!(guide.contains("ICMP datagram sockets are open to every user"));
            assert!(available("4"), "the guide says ICMP needs nothing on macOS, and doctor says it is unavailable");
            assert!(said.contains("echo identifier preserved") && said.contains("ICMP errors     delivered on the socket"));
        }
        Some("linux") => {
            let range = report["ping_group_range"].as_str().expect("the range in force");
            let bounds: Vec<u64> = range.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            let within = groups.iter().any(|g| bounds.len() == 2 && bounds[0] <= *g && *g <= bounds[1]);
            assert_eq!(available("4"), within, "ICMP is available exactly when a group id lies in the range {range:?} (groups {groups:?})");
            assert!(said.contains("echo identifier rewritten by the kernel") && said.contains("ICMP errors     read from the socket error queue"));
            if !within {
                assert!(said.contains("UNSUPPORTED") && said.contains("net.ipv4.ping_group_range = 0 2147483647"), "{said}");
            }
        }
        other => panic!("an operating system the guide does not cover: {other:?}"),
    }
    // The remedy is quoted as the program prints it, and the guide's promise holds.
    assert!(guide.contains("net.ipv4.ping_group_range = 0 2147483647"));
    assert!(said.contains("Nothing was sent."));
    for transport in ["DNS over UDP", "DNS over TCP", "DNS over TLS", "DNS over HTTPS"] {
        assert!(said.contains(&format!("{transport:<15} available (no privilege needed)")), "{transport}: {said}");
    }
    assert_eq!(report["running_as_root"] == true, said.contains("warning         running as root"));
}

/// The README points at files that exist and names commands the program has.
#[test]
fn the_readme_links_to_files_that_exist_and_names_real_commands() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md");
    let mut links = 0;
    for target in readme.split("](").skip(1).filter_map(|rest| rest.split(')').next()).filter(|t| !t.starts_with("http") && !t.starts_with('#')) {
        assert!(root.join(target).exists(), "README links to {target}, which does not exist");
        links += 1;
    }
    assert!(links >= 8, "{links} links found");
    let page = doc("pathwatch.1");
    for row in readme.lines().filter(|l| l.starts_with("| `pathwatch ")) {
        let command: Vec<&str> = row.split('`').nth(1).unwrap_or("").split_whitespace().skip(1).take_while(|w| !w.starts_with(['[', '<'])).collect();
        let name = command.join(" ").trim_end_matches(',').to_string();
        assert!(page.contains(&format!(".SS pathwatch {name}\n")), "README names `pathwatch {name}`, which the manual page does not have");
    }
    // The traffic it promises by default is the traffic the default plan has.
    let plan = pathwatch(&["config", "check"]);
    assert!(
        plan.contains("16 stream(s) would collect") && plan.contains("steady-state rate: 5.61 probe transactions per second"),
        "the README says \"about six transactions a second\": {plan}"
    );
}

//! Every word the engine can make the page say, in both languages.
//!
//! The page builds some keys from values, `t(`beatbox.lifecycle.${state}`)`,
//! so no scan of the page sees a new state: it would render as the raw key
//! and nothing would fail. So the families are expanded here from the
//! engine's tables and looked up in `locales/`. The page's own half is
//! `web/tests/strings.test.mjs`.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use beatbox_engine::runtime::isolation::ISOLATION_LEVELS;
use beatbox_engine::runtime::lifecycle::{CALLABLE, LIFECYCLES};
use beatbox_engine::runtime::resources::RESOURCE_KINDS;
use beatbox_host::HostSandbox;
use beatbox_isolation::machine::NETWORK_MODES;
use serde_json::json;
use support::{Room, call, data, open};

const LANGUAGES: [&str; 2] = ["en", "pt_PT"];

fn catalogues() -> Vec<(&'static str, BTreeMap<String, String>)> {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../locales");
    LANGUAGES
        .into_iter()
        .map(|language| {
            let path = folder.join(format!("{language}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            (language, serde_json::from_str(&text).unwrap())
        })
        .collect()
}

fn missing(keys: &[String]) -> Vec<String> {
    let catalogues = catalogues();
    keys.iter()
        .flat_map(|key| {
            catalogues
                .iter()
                .filter(|(_, strings)| !strings.contains_key(key))
                .map(move |(language, _)| format!("{key} (missing from {language}.json)"))
        })
        .collect()
}

#[test]
fn test_every_word_the_sandbox_screen_computes_exists() {
    let mut keys = Vec::new();
    for state in LIFECYCLES {
        keys.push(format!("beatbox.lifecycle.{}", state.as_str()));
    }
    for transition in CALLABLE {
        keys.push(format!("beatbox.transition.{}", transition.as_str()));
    }
    for level in ISOLATION_LEVELS {
        keys.push(format!("beatbox.isolation.{}", level.as_str()));
    }
    for kind in RESOURCE_KINDS {
        keys.push(format!("beatbox.resource.{}", kind.as_str()));
    }
    for mode in NETWORK_MODES {
        keys.push(format!("beatbox.network.{mode}"));
        keys.push(format!("beatbox.network.{mode}.what"));
    }
    assert_eq!(missing(&keys), Vec::<String>::new());
}

/// Nobody should have to read `lifecycle.reset`: a word for every state,
/// transition, boundary and reach the page shows.
#[test]
fn test_the_sandbox_screen_can_say_all_of_it_in_words() {
    let mut keys = Vec::new();
    for state in LIFECYCLES {
        keys.push(format!("beatbox.plain.state.{}", state.as_str()));
        keys.push(format!("beatbox.plain.is.{}", state.as_str()));
    }
    for transition in CALLABLE {
        keys.push(format!("beatbox.plain.do.{}", transition.as_str()));
    }
    for level in ISOLATION_LEVELS {
        keys.push(format!("beatbox.plain.boundary.{}", level.as_str()));
    }
    for mode in NETWORK_MODES {
        keys.push(format!("beatbox.plain.reach.{mode}"));
        keys.push(format!("beatbox.reach.{mode}"));
        keys.push(format!("beatbox.reach.{mode}.what"));
    }
    // Every kind the daemon registers has a word, not its identifier.
    keys.push(format!("beatbox.kind.{}", HostSandbox::spec().name));
    assert_eq!(missing(&keys), Vec::<String>::new());
}

/// Reason codes the probe names through a helper, not `reason!`, so a scan
/// can't see them: asked, not read.
#[test]
fn test_every_probe_names_a_reason_that_exists_in_both_languages() {
    let room = Room::new("strings");
    let api = open(room.path());
    let probe = data(call(&api, "system.probe", json!({})));
    let mut keys = Vec::new();
    for name in ["kvm", "qemu", "frida", "terminal"] {
        let reason = &probe[name]["reason"];
        let code = reason["code"].as_str().unwrap_or_default();
        assert!(!code.is_empty(), "the {name} probe explains nothing");
        assert!(
            !code.contains(' '),
            "{name} hands over a sentence: {code:?}"
        );
        let fields: Vec<&str> = reason
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(fields, ["code", "values"], "{name}");
        keys.push(format!("beatbox.{code}"));
    }
    assert_eq!(missing(&keys), Vec::<String>::new());
}

/// The page draws a button for each transition and a pill for each state from
/// its own two lists (`web/src/bridge/types.ts`), and the words above are
/// checked against the engine's. The two lists have to be the engine's.
#[test]
fn test_the_page_knows_the_states_and_transitions_the_engine_has() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/src/bridge/types.ts");
    let source = std::fs::read_to_string(&path).unwrap();
    let list = |name: &str| -> Vec<String> {
        let start = format!("export const {name} = [");
        let from = source
            .find(&start)
            .unwrap_or_else(|| panic!("types.ts has no {name}"))
            + start.len();
        let body = &source[from..from + source[from..].find(']').unwrap()];
        body.split(',')
            .map(|word| word.trim().trim_matches('\'').to_owned())
            .filter(|word| !word.is_empty())
            .collect()
    };
    let engine = |words: Vec<&str>| words.into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        list("TRANSITIONS"),
        engine(
            CALLABLE
                .iter()
                .map(|transition| transition.as_str())
                .collect()
        )
    );
    assert_eq!(
        list("LIFECYCLES"),
        engine(LIFECYCLES.iter().map(|state| state.as_str()).collect())
    );
}

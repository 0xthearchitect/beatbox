//! The Service Definition: what is refused when it is loaded, and how a
//! configuration is checked against it.

use beatbox_engine::{Reason, data};
use beatbox_server::definition::{Port, SECRET_SHOWN};
use beatbox_server::{Definition, builtins};
use serde_json::{Value, json};

/// A process service with a bit of everything, valid as it is.
fn custom() -> Value {
    json!({
        "id": "custom-erp",
        "version": "1",
        "name": "ERP",
        "runtime": {"type": "process", "command": ["/service/erp", "--port={port}", "--mode", "{mode}"]},
        "configuration": {
            "port": {"kind": "number", "default": 3610},
            "mode": {"kind": "enum", "values": ["test", "live"], "default": "test"},
            "api_key": {"kind": "secret"},
            "title": {"kind": "text", "default": "ERP"}
        },
        "interfaces": [{"transport": "tcp", "port": "{port}"}],
        "files": [{"path": "/etc/erp/{_id}.json", "content": "{\"title\": \"{title}\"}"}],
        "health": {"type": "tcp", "port": "{port}"},
        "capabilities": [{
            "capability": "erp.invoice.create",
            "inputs": [{"name": "amount", "kind": "number"}],
            "outputs": [{"name": "exit_code", "kind": "number"}],
            "run": ["/service/erp-cli", "invoice", "{amount}", "--key", "{api_key}"]
        }]
    })
}

fn with(mut base: Value, path: &[&str], value: Value) -> Value {
    let (last, parents) = path.split_last().unwrap();
    let mut at = &mut base;
    for key in parents {
        at = if let Ok(index) = key.parse::<usize>() {
            &mut at[index]
        } else {
            &mut at[*key]
        };
    }
    match last.parse::<usize>() {
        Ok(index) => at[index] = value,
        Err(_) => at[*last] = value,
    }
    base
}

fn refused(raw: Value) -> Reason {
    Definition::from_value(&raw).expect_err("refused")
}

fn problem(raw: Value) -> String {
    let why = refused(raw);
    assert_eq!(why.code, "server.bad_definition", "{why:?}");
    why.values["problem"].clone()
}

#[test]
fn the_built_in_ones_load_and_offer_web_dns_database_and_email() {
    let all = builtins();
    let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["http-static", "nginx", "dns", "postgresql", "smtp"]);
    assert!(all[2].is_internal());
    assert!(!all[0].is_internal());
    assert_eq!(all[2].specs().len(), 3);
}

#[test]
fn a_valid_one_loads_and_round_trips() {
    let definition = Definition::from_value(&custom()).unwrap();
    let again = Definition::from_value(&serde_json::to_value(&definition).unwrap()).unwrap();
    assert_eq!(definition, again);
}

#[test]
fn website_offers_three_modes_and_embeds_its_runner() {
    let web = &builtins()[0];
    assert_eq!(web.version, "5");
    assert_eq!(
        web.configuration["mode"].values,
        ["page", "folder", "application"]
    );
    assert_eq!(
        web.files[0].content,
        // Windows checkouts may use CRLF; the embedded guest script uses LF.
        include_str!("../definitions/web-run.py").replace("\r\n", "\n")
    );
    assert!(web.prepare.is_some());
    let old: Value = json!({"id":"legacy-web","version":"1","name":"Legacy web",
        "runtime":{"type":"process","command":["python3","-m","http.server"]}});
    assert!(Definition::from_value(&old).unwrap().prepare.is_none());
}

#[test]
fn database_and_nginx_embed_their_canonical_preparation_runners() {
    let definitions = builtins();
    for (index, reference) in [
        (1, include_str!("../definitions/nginx-run.py")),
        (3, include_str!("../definitions/postgresql-run.sh")),
    ] {
        assert_eq!(
            definitions[index].files[0].content,
            reference.replace("\r\n", "\n")
        );
        assert!(definitions[index].prepare.is_some());
    }
}

#[test]
fn preparation_commands_are_checked_like_other_definition_commands() {
    let mut raw = custom();
    raw["prepare"] = json!([]);
    assert!(problem(raw.clone()).contains("needs a command"));
    raw["prepare"] = json!(["check", "{unknown}"]);
    assert!(problem(raw.clone()).contains("{unknown}"));
    raw["prepare"] = json!(["check", "{port}"]);
    assert!(Definition::from_value(&raw).is_ok());
}

#[test]
fn a_placeholder_that_names_nothing_is_refused_on_load() {
    let raw = with(
        custom(),
        &["runtime", "command", "1"],
        json!("--port={prot}"),
    );
    assert!(problem(raw).contains("{prot}"));
    let raw = with(custom(), &["files", "0", "path"], json!("/etc/{nothing}"));
    assert!(problem(raw).contains("{nothing}"));
}

#[test]
fn braces_that_are_not_a_placeholder_are_text() {
    // custom()'s file content is JSON, braces and all, and it loads.
    assert!(Definition::from_value(&custom()).is_ok());
}

#[test]
fn a_run_may_use_its_inputs_and_nothing_else() {
    let raw = with(
        custom(),
        &["capabilities", "0", "run", "2"],
        json!("{currency}"),
    );
    assert!(problem(raw).contains("{currency}"));
}

#[test]
fn an_internal_service_on_tcp_is_refused_by_name() {
    let raw = json!({
        "id": "dns-tcp", "version": "1", "name": "DNS",
        "runtime": {"type": "internal", "implementation": "dns"},
        "interfaces": [{"transport": "tcp", "port": 53}]
    });
    let why = refused(raw);
    assert_eq!(why.code, "server.internal_udp_only");
    assert_eq!(why.values["definition"], "dns-tcp");
}

#[test]
fn every_other_mistake_is_refused_with_what_it_is() {
    let cases = [
        (with(custom(), &["id"], json!("Custom ERP")), "its id"),
        // The id names its file in the library: never a path out of it.
        (with(custom(), &["id"], json!("../../.bashrc")), "its id"),
        (with(custom(), &["id"], json!("a/b")), "its id"),
        (
            with(custom(), &["runtime", "command"], json!([])),
            "needs a command",
        ),
        (
            with(
                custom(),
                &["runtime"],
                json!({"type": "internal", "implementation": "smtp"}),
            ),
            "no internal implementation called smtp",
        ),
        (
            with(custom(), &["configuration", "mode", "values"], json!([])),
            "enum with no values",
        ),
        (
            with(
                custom(),
                &["configuration", "api_key", "default"],
                json!("x"),
            ),
            "secret has no default",
        ),
        (
            with(
                custom(),
                &["configuration", "port", "default"],
                json!("eighty"),
            ),
            "not a number",
        ),
        (
            with(custom(), &["interfaces", "0", "port"], json!("http")),
            "not a port",
        ),
        (
            with(custom(), &["health"], json!({"type": "internal"})),
            "only an internal service",
        ),
        (
            with(
                custom(),
                &["capabilities", "0", "capability"],
                json!("invoice"),
            ),
            "namespace.verb",
        ),
        (
            with(
                custom(),
                &["capabilities", "0", "capability"],
                json!("service.start"),
            ),
            "keeps for itself",
        ),
        (
            with(custom(), &["capabilities", "0", "run"], Value::Null),
            "needs a run",
        ),
        (
            with(
                custom(),
                &["capabilities", "0", "inputs", "0", "kind"],
                json!("money"),
            ),
            "not a field",
        ),
        (with(custom(), &["colour"], json!("red")), "unknown field"),
        (
            with(custom(), &["requires"], json!(["python3", "rm -rf /"])),
            "not a program it can require",
        ),
    ];
    for (raw, expected) in cases {
        let said = problem(raw);
        assert!(said.contains(expected), "{expected:?} not in {said:?}");
    }
}

#[test]
fn a_capability_described_twice_is_refused() {
    let mut raw = custom();
    let again = raw["capabilities"][0].clone();
    raw["capabilities"].as_array_mut().unwrap().push(again);
    assert!(problem(raw).contains("described twice"));
}

#[test]
fn an_internal_capability_is_done_in_code_not_with_a_run() {
    let raw = json!({
        "id": "dns-run", "version": "1", "name": "DNS",
        "runtime": {"type": "internal", "implementation": "dns"},
        "capabilities": [{"capability": "dns.flush", "run": ["true"]}]
    });
    assert!(problem(raw).contains("not with a run"));
}

mod configuration {
    use super::*;

    fn erp() -> Definition {
        Definition::from_value(&custom()).unwrap()
    }

    #[test]
    fn defaults_fill_what_is_not_given() {
        let config = erp()
            .configure("billing", &data!({"api_key": "k"}))
            .unwrap();
        assert_eq!(config["port"], 3610);
        assert_eq!(config["mode"], "test");
        assert_eq!(config["api_key"], "k");
    }

    #[test]
    fn each_wrong_key_is_refused_by_name() {
        let cases = [
            (
                data!({"api_key": "k", "prot": 1}),
                "server.config_unknown",
                "prot",
            ),
            (data!({}), "server.config_missing", "api_key"),
            (
                data!({"api_key": "k", "port": "80"}),
                "server.config_wrong_kind",
                "port",
            ),
            (
                data!({"api_key": "k", "mode": "staging"}),
                "server.config_not_allowed",
                "mode",
            ),
            (data!({"api_key": 7}), "server.config_wrong_kind", "api_key"),
        ];
        for (given, code, key) in cases {
            let why = erp().configure("billing", &given).unwrap_err();
            assert_eq!((why.code.as_str(), why.values["key"].as_str()), (code, key));
            assert_eq!(why.values["service"], "billing");
        }
    }

    #[test]
    fn text_takes_a_number_as_the_editor_writes_it() {
        let config = erp()
            .configure("billing", &data!({"api_key": "k", "title": 2024}))
            .unwrap();
        assert_eq!(config["title"], 2024);
    }

    #[test]
    fn values_are_text_and_ports_resolve() {
        let definition = erp();
        let config = definition
            .configure("billing", &data!({"api_key": "k", "port": 8080}))
            .unwrap();
        let values = definition.values("billing", "svc-1", &config);
        assert_eq!(values["port"], "8080");
        assert_eq!(values["_name"], "billing");
        assert_eq!(values["_id"], "svc-1");
        assert_eq!(definition.interfaces[0].port.resolve(&values), Some(8080));
        assert_eq!(Port::Text("{port}9".into()).resolve(&values), None);
        assert_eq!(Port::Number(53).resolve(&values), Some(53));
    }

    #[test]
    fn a_secret_is_never_shown_back() {
        let definition = erp();
        let config = definition
            .configure("billing", &data!({"api_key": "hunter2"}))
            .unwrap();
        let shown = definition.masked(&config);
        assert_eq!(shown["api_key"], SECRET_SHOWN);
        assert_eq!(shown["port"], 3610);
        assert!(!serde_json::to_string(&shown).unwrap().contains("hunter2"));
    }

    #[test]
    fn a_capability_checks_its_inputs_as_an_action_does() {
        let spec = erp().spec("erp.invoice.create").unwrap();
        let why = spec.check_inputs(&data!({"amount": "ten"})).unwrap_err();
        assert_eq!(why.code, "engine.bad_argument");
        assert!(spec.check_inputs(&data!({"amount": 10})).is_ok());
        assert!(erp().spec("erp.refund").is_none());
    }
}

#[test]
fn an_input_named_like_a_setting_is_refused() {
    let raw = with(
        custom(),
        &["capabilities", "0", "inputs", "0", "name"],
        json!("mode"),
    );
    let raw = with(raw, &["capabilities", "0", "run", "2"], json!("{mode}"));
    assert!(problem(raw).contains("also a setting"));
}

/// What `delete_data` may delete: only an absolute path cut just after a
/// `{_id}` segment. A file named after the id, or a path that starts with a
/// setting, is never taken as the instance's own.
#[test]
fn an_instances_own_folders_are_the_ones_named_by_its_id() {
    let mut raw = custom();
    raw["runtime"]["command"] = json!([
        "/opt/app/{_id}/bin/run",
        "--data=/var/lib/app/{_id}",
        "/var/lib/app/{_id}/cache"
    ]);
    raw["files"] = json!([
        {"path": "/etc/app/{_id}.conf", "content": "x"},
        {"path": "/etc/app/{_id}/more.conf", "content": "x"}
    ]);
    let definition = Definition::from_value(&raw).unwrap();
    assert_eq!(
        definition.own_folders(),
        ["/etc/app/{_id}", "/opt/app/{_id}", "/var/lib/app/{_id}"]
    );
}

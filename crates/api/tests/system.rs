//! What the host says about itself.
//!
//! The language half is the shell's.

mod support;

use beatbox_api::system::{KvmFacts, kvm};
use beatbox_contract::SystemProbe;
use serde_json::json;
use support::{Room, call, data, open};

/// And names the explanation, so it can be read in either language.
#[test]
fn test_every_capability_explains_itself() {
    let room = Room::new("probe");
    let api = open(room.path());
    let probe = data(call(&api, "system.probe", json!({})));
    for name in ["kvm", "qemu", "frida", "terminal"] {
        let availability = &probe[name];
        assert!(availability["ready"].is_boolean(), "{name}");
        let code = availability["reason"]["code"].as_str().unwrap_or_default();
        assert!(
            code.starts_with(&format!("probe.{name}.")),
            "{name} says nothing a reader can act on: {code:?}"
        );
    }
    // And it is exactly the contract's shape, which refuses a field it does
    // not know and one it is missing.
    let _: SystemProbe = serde_json::from_value(probe).unwrap();
}

fn linux(usable: bool, device: bool) -> KvmFacts {
    KvmFacts {
        usable,
        windows: false,
        whpx: false,
        linux: true,
        device,
    }
}

/// Debian, Ubuntu and Fedora give /dev/kvm to the kvm group only. Say which
/// problem it is: the fixes have nothing in common.
#[test]
fn test_there_but_not_ours_says_to_join_the_group() {
    assert_eq!(
        kvm(linux(false, true)).reason.code,
        "probe.kvm.no_permission"
    );
}

#[test]
fn test_not_there_at_all_says_so() {
    assert_eq!(kvm(linux(false, false)).reason.code, "probe.kvm.missing");
}

#[test]
fn test_windows_asks_about_whpx_not_kvm() {
    let windows = |whpx| KvmFacts {
        usable: false,
        windows: true,
        whpx,
        linux: false,
        device: false,
    };
    assert_eq!(kvm(windows(false)).reason.code, "probe.kvm.whpx_missing");
    assert!(kvm(windows(true)).ready);
}

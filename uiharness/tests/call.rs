use ctlcore::json::Json;
use uiharness::script::{Action, parse_script};

#[test]
fn raw_call_preserves_json_and_rejects_bad_requests_before_launch() {
    let script = parse_script(
        "test raw call\n call task.add {\"name\":\"Task #1\",\"path\":\"a\\\\#b.xml\"}\n",
    )
    .unwrap();
    let Action::Call { verb, args } = &script.cases[0].steps[0].action else {
        panic!("call")
    };
    assert_eq!(verb, "task.add");
    assert_eq!(args.get_str("name"), Some("Task #1"));
    assert_eq!(args.get_str("path"), Some("a\\#b.xml"));
    for step in [
        "call",
        "call task.add",
        "call task.add {",
        "call task.add []",
        "call task.add null",
        "call task.add true",
        "call task.add {} # trailing comment",
    ] {
        assert!(
            parse_script(&format!("test bad\n {step}\n")).is_err(),
            "{step}"
        );
    }
    let script = parse_script("test empty args\n call task.list {}\n").unwrap();
    assert_eq!(
        script.cases[0].steps[0].action,
        Action::Call {
            verb: "task.list".into(),
            args: Json::Obj(vec![])
        }
    );
}

use super::*;
use pretty_assertions::assert_eq;

fn parse(arguments: &[&str]) -> Result<StorageCommand, clap::Error> {
    let mut full = vec!["storage"];
    full.extend_from_slice(arguments);
    StorageCommand::try_parse_from(full)
}

#[test]
fn start_needs_a_plan_and_names_its_confirmations() {
    assert!(parse(&["start", "migrate"]).is_err());
    let command = parse(&[
        "start",
        "migrate",
        "--plan-id",
        "0194e0a0-0000-7000-8000-000000000001",
        "--yes",
        "--writers-stopped",
        "--activate",
        "--json",
    ])
    .expect("a fully confirmed start parses");
    let StorageSubcommand::Start {
        action,
        yes,
        writers_stopped,
        activate,
        json,
        operation_id,
        ..
    } = command.subcommand
    else {
        panic!("expected a start command");
    };
    assert!(matches!(action, ActionArg::Migrate));
    assert!(yes && writers_stopped && activate && json);
    assert_eq!(operation_id, None);
}

#[test]
fn credentials_are_never_arguments() {
    assert!(parse(&["credential", "set", "team-postgres"]).is_ok());
    assert!(parse(&["credential", "set", "team-postgres", "--value", "hunter2"]).is_err());
    assert!(parse(&["credential", "set"]).is_err());
}

#[test]
fn blocked_output_is_stable() {
    let blocked = Blocked {
        ok: false,
        blocker: BlockerCode::StalePlan.as_str(),
        retryable: BlockerCode::StalePlan.is_retryable(),
    };
    assert_eq!(
        serde_json::to_value(&blocked).expect("json"),
        serde_json::json!({"ok": false, "blocker": "stale_plan", "retryable": false})
    );
    assert!(BlockerCode::DatasetMigrating.is_retryable());
}

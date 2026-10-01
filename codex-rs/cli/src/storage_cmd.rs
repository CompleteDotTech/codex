//! `codex storage`: the headless front end of the storage service.
//!
//! Every subcommand calls the same service the app-server and the TUI use, prints either a short
//! human summary or JSON, and exits with a code a script can act on: 0 on success, 2 when an
//! operation is blocked, and 3 when it is blocked for a reason that may pass on its own.

use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;
use codex_core::config::Config;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use codex_storage_authority::CredentialRef;
use codex_storage_authority::KEYRING_SERVICE;
use codex_storage_authority::StorageCandidateProfile;
use codex_storage_service::BlockerCode;
use codex_storage_service::Confirmation;
use codex_storage_service::OperationRecord;
use codex_storage_service::PlanAction;
use codex_storage_service::StorageError;
use codex_storage_service::StorageService;
use codex_storage_service::StorageServiceInputs;
use codex_utils_cli::CliConfigOverrides;
use serde::Serialize;
use std::io::IsTerminal;
use std::io::Read;
use std::sync::Arc;
use uuid::Uuid;

const EXIT_BLOCKED: i32 = 2;
const EXIT_RETRYABLE: i32 = 3;
const MAX_CREDENTIAL_BYTES: usize = 8192;

#[derive(Debug, Parser)]
pub(crate) struct StorageCommand {
    #[command(subcommand)]
    subcommand: StorageSubcommand,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ActionArg {
    /// Copy this home into the remote dataset and make it authoritative.
    Migrate,
    /// Copy the remote dataset back into this home and make local files authoritative.
    Return,
    /// Join a dataset that already exists. Nothing is copied or merged.
    Attach,
}

impl From<ActionArg> for PlanAction {
    fn from(action: ActionArg) -> Self {
        match action {
            ActionArg::Migrate => Self::Migrate,
            ActionArg::Return => Self::Return,
            ActionArg::Attach => Self::Attach,
        }
    }
}

#[derive(Debug, Subcommand)]
enum StorageSubcommand {
    /// Show the active backend and what blocks changing it.
    Status {
        /// Also connect to the saved remote profile and report the dataset.
        #[arg(long)]
        probe: bool,
        #[arg(long)]
        json: bool,
    },
    /// Test the saved remote profile without changing anything.
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Create or upgrade the remote tables with the schema-owner credential.
    Initialize {
        #[arg(long)]
        json: bool,
    },
    /// Preview a migration or a return and list what blocks it.
    Plan {
        #[arg(value_enum)]
        action: ActionArg,
        #[arg(long)]
        json: bool,
    },
    /// Copy and verify. Add `--activate` to also make the copy authoritative.
    Start {
        #[arg(value_enum)]
        action: ActionArg,
        /// The plan that was previewed; a changed world makes it stale.
        #[arg(long)]
        plan_id: Uuid,
        /// Names the operation; repeating a request with the same id never starts a second copy.
        #[arg(long)]
        operation_id: Option<Uuid>,
        /// Confirm that you read the plan.
        #[arg(long)]
        yes: bool,
        /// The dataset to join; required for `attach`, copied from the plan you previewed.
        #[arg(long)]
        dataset_id: Option<Uuid>,
        /// Confirm that every Codex process writing this home is stopped.
        #[arg(long)]
        writers_stopped: bool,
        #[arg(long)]
        activate: bool,
        #[arg(long)]
        json: bool,
    },
    /// Make a verified copy authoritative.
    Activate {
        operation_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Settle an interrupted cutover from the evidence on both sides.
    Recover {
        #[arg(long)]
        json: bool,
    },
    /// Cancel an operation that has not been activated.
    Cancel {
        operation_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// List recorded operations.
    Operations {
        #[arg(long)]
        json: bool,
    },
    /// Manage the credentials the saved profile refers to.
    Credential {
        #[command(subcommand)]
        subcommand: CredentialSubcommand,
    },
}

#[derive(Debug, Subcommand)]
enum CredentialSubcommand {
    /// Store a credential under an id the profile can refer to. The secret is read from standard
    /// input and is never accepted as an argument or printed.
    Set { id: String },
}

#[derive(Serialize)]
struct Blocked {
    ok: bool,
    blocker: &'static str,
    retryable: bool,
}

fn print_json(value: &impl Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Exit with the code for a blocker whose details were already printed.
fn exit_for(code: BlockerCode) -> ! {
    std::process::exit(if code.is_retryable() {
        EXIT_RETRYABLE
    } else {
        EXIT_BLOCKED
    });
}

/// Report a refusal the way scripts and people both can read it, then exit.
fn exit_blocked(error: StorageError, json: bool) -> ! {
    let code = error.0;
    if json {
        let _ = print_json(&Blocked {
            ok: false,
            blocker: code.as_str(),
            retryable: code.is_retryable(),
        });
    } else {
        eprintln!("blocked: {}", code.as_str());
    }
    std::process::exit(if code.is_retryable() {
        EXIT_RETRYABLE
    } else {
        EXIT_BLOCKED
    });
}

fn host_label() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

async fn service(config_overrides: CliConfigOverrides) -> anyhow::Result<StorageService> {
    let overrides = config_overrides
        .parse_overrides()
        .map_err(|error| anyhow::anyhow!("invalid -c override: {error}"))?;
    let config = Config::load_with_cli_overrides(overrides).await?;
    let candidate = match config.config_layer_stack.storage_candidate()? {
        Some(StorageCandidateProfile::RemotePostgres(profile)) => Some(profile),
        Some(StorageCandidateProfile::LocalSqlite) | None => None,
    };
    Ok(StorageService::new(StorageServiceInputs {
        codex_home: config.codex_home.to_path_buf(),
        sqlite: config.sqlite_config().clone(),
        candidate,
        default_model_provider_id: config.model_provider_id.clone(),
        host_label: host_label(),
        keyring: Arc::new(DefaultKeyringStore),
    }))
}

fn describe(record: &OperationRecord) -> String {
    format!(
        "operation {} ({:?}): {:?}{}",
        record.operation_id,
        record.action,
        record.state,
        record
            .blocker
            .map(|blocker| format!(" [{}]", blocker.as_str()))
            .unwrap_or_default()
    )
}

fn set_credential(id: String) -> anyhow::Result<()> {
    let id = CredentialRef::parse(id)
        .map_err(|_| anyhow::anyhow!("credential ids use letters, digits, '-' and '_' only"))?;
    if std::io::stdin().is_terminal() {
        anyhow::bail!("pipe the credential on standard input; it is never accepted as an argument");
    }
    let mut secret = String::new();
    std::io::stdin()
        .take(MAX_CREDENTIAL_BYTES as u64 + 1)
        .read_to_string(&mut secret)?;
    let secret = secret.trim_end_matches(['\r', '\n']);
    if secret.is_empty() || secret.len() > MAX_CREDENTIAL_BYTES {
        anyhow::bail!("the credential must be between 1 and {MAX_CREDENTIAL_BYTES} bytes");
    }
    DefaultKeyringStore
        .save(KEYRING_SERVICE, id.as_str(), secret)
        .map_err(|_| anyhow::anyhow!("the credential could not be stored"))?;
    println!("stored credential {}", id.as_str());
    Ok(())
}

pub(crate) async fn run_storage_command(
    command: StorageCommand,
    config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    if let StorageSubcommand::Credential {
        subcommand: CredentialSubcommand::Set { id },
    } = command.subcommand
    {
        return set_credential(id);
    }
    let service = service(config_overrides).await?;
    match command.subcommand {
        StorageSubcommand::Credential { .. } => Ok(()),
        StorageSubcommand::Status { probe, json } => {
            let status = service.status(probe).await;
            if json {
                return print_json(&status);
            }
            println!("host: {}", service.host_label());
            println!("active backend: {:?}", status.active_backend);
            println!("authority: {:?}", status.authority);
            for blocker in &status.blockers {
                println!("blocker: {}", blocker.as_str());
            }
            Ok(())
        }
        StorageSubcommand::Check { json } => {
            let report = service.check_connection().await;
            if json {
                print_json(&report)?;
            } else {
                println!("stage: {:?}", report.stage);
            }
            match report.blocker {
                Some(blocker) => exit_for(blocker),
                None => Ok(()),
            }
        }
        StorageSubcommand::Initialize { json } => match service.initialize_schema().await {
            Ok(format) => {
                if json {
                    print_json(&serde_json::json!({"ok": true, "schema_format": format}))
                } else {
                    println!("schema format {format} is ready");
                    Ok(())
                }
            }
            Err(error) => exit_blocked(error, json),
        },
        StorageSubcommand::Plan { action, json } => {
            let plan = service
                .plan(action.into())
                .await
                .unwrap_or_else(|error| exit_blocked(error, json));
            if json {
                print_json(&plan)?;
            } else {
                println!(
                    "plan {} for {:?} on {}",
                    plan.plan_id, plan.action, plan.host
                );
                println!("destination: {}", plan.destination);
                for blocker in &plan.blockers {
                    println!("blocker: {}", blocker.as_str());
                }
            }
            match plan.blockers.first() {
                Some(blocker) => exit_for(*blocker),
                None => Ok(()),
            }
        }
        StorageSubcommand::Start {
            action,
            plan_id,
            operation_id,
            yes,
            dataset_id,
            writers_stopped,
            activate,
            json,
        } => {
            if !yes {
                exit_blocked(StorageError(BlockerCode::NotConfirmed), json);
            }
            let operation_id = operation_id.unwrap_or_else(Uuid::new_v4);
            let confirmation = Confirmation { writers_stopped };
            let started = match PlanAction::from(action) {
                PlanAction::Migrate => {
                    service
                        .start_migration(operation_id, plan_id, confirmation)
                        .await
                }
                PlanAction::Return => {
                    service
                        .start_return(operation_id, plan_id, confirmation)
                        .await
                }
                PlanAction::Attach => match dataset_id {
                    Some(dataset_id) => {
                        service
                            .attach_dataset(operation_id, plan_id, dataset_id, confirmation)
                            .await
                    }
                    None => Err(StorageError(BlockerCode::NotConfirmed)),
                },
            };
            let mut record = started.unwrap_or_else(|error| exit_blocked(error, json));
            if activate {
                record = service
                    .activate(record.operation_id)
                    .await
                    .unwrap_or_else(|error| exit_blocked(error, json));
            }
            if json {
                print_json(&record)
            } else {
                println!("{}", describe(&record));
                Ok(())
            }
        }
        StorageSubcommand::Activate { operation_id, json } => {
            let record = service
                .activate(operation_id)
                .await
                .unwrap_or_else(|error| exit_blocked(error, json));
            if json {
                print_json(&record)
            } else {
                println!("{}", describe(&record));
                Ok(())
            }
        }
        StorageSubcommand::Recover { json } => {
            let report = service
                .recover()
                .await
                .unwrap_or_else(|error| exit_blocked(error, json));
            if json {
                print_json(&report)
            } else {
                println!("recovery: {:?}", report.outcome);
                for record in &report.operations {
                    println!("{}", describe(record));
                }
                Ok(())
            }
        }
        StorageSubcommand::Cancel { operation_id, json } => {
            let record = service
                .cancel(operation_id)
                .await
                .unwrap_or_else(|error| exit_blocked(error, json));
            if json {
                print_json(&record)
            } else {
                println!("{}", describe(&record));
                Ok(())
            }
        }
        StorageSubcommand::Operations { json } => {
            let operations = service.operations();
            if json {
                print_json(&operations)
            } else {
                for record in &operations {
                    println!("{}", describe(record));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_cmd_tests.rs"]
mod tests;

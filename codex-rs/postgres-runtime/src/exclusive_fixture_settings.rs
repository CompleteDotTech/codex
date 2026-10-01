use crate::ConnectionSettings;
use crate::PoolLimits;
use crate::exclusive_fixture_io::capture;
use crate::exclusive_fixture_io::read_file;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub(crate) async fn settings(state: &Path, role: &str) -> Result<ConnectionSettings, &'static str> {
    settings_with_inspection(state, role, |context, args| async move {
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        docker_identity(&context, &args).await
    })
    .await
}

async fn settings_with_inspection<F, Fut>(
    state: &Path,
    role: &str,
    mut inspect_identity: F,
) -> Result<ConnectionSettings, &'static str>
where
    F: FnMut(String, Vec<String>) -> Fut + Send,
    Fut: std::future::Future<Output = Result<std::process::Output, &'static str>> + Send,
{
    let receipt: Value = serde_json::from_slice(&read_file(
        &state.join("receipt.json"),
        /*limit*/ 65_536,
    )?)
    .map_err(|_| "fixture_receipt_invalid")?;
    let instance = receipt["instance"]
        .as_str()
        .ok_or("fixture_instance_invalid")?;
    if instance.len() != 32
        || !instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("fixture_instance_invalid");
    }
    let exclusive: Value = serde_json::from_slice(&read_file(
        &state.join("postgres-fixture-exclusive.json"),
        /*limit*/ 65_536,
    )?)
    .map_err(|_| "exclusive_receipt_invalid")?;
    if exclusive["instance"].as_str() != Some(instance)
        || exclusive["purpose"].as_str() != Some("postgres-runtime-fixture-exclusive")
        || exclusive["disposable"].as_bool() != Some(true)
    {
        return Err("exclusive_receipt_mismatch");
    }
    let context = exclusive["docker_context"]
        .as_str()
        .ok_or("docker_context_invalid")?;
    if context.is_empty()
        || context.len() > 48
        || !context
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err("docker_context_invalid");
    }
    let container = exclusive["container_id"]
        .as_str()
        .ok_or("fixture_container_invalid")?;
    if container.len() != 64
        || !container
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("fixture_container_invalid");
    }
    let project = receipt["project"]
        .as_str()
        .ok_or("fixture_project_invalid")?;
    if !project.starts_with("codex-pg-")
        || project.len() > 49
        || project.len() <= 9
        || !project
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("fixture_project_invalid");
    }
    let engine_id = receipt["engine_id"]
        .as_str()
        .ok_or("fixture_engine_invalid")?;
    if engine_id.is_empty() {
        return Err("fixture_engine_invalid");
    }
    let engine = inspect_identity(
        context.to_string(),
        vec![
            "info".to_string(),
            "--format".to_string(),
            "{{.ID}}".to_string(),
        ],
    )
    .await?;
    if !engine.status.success()
        || std::str::from_utf8(&engine.stdout)
            .map_err(|_| "fixture_engine_invalid")?
            .trim()
            != engine_id
    {
        return Err("fixture_engine_mismatch");
    }
    let inspect = inspect_identity(
        context.to_string(),
        vec!["inspect".to_string(), container.to_string()],
    )
    .await?;
    if !inspect.status.success() || inspect.stdout.len() >= 1_048_576 {
        return Err("fixture_inspection_invalid");
    }
    let containers: Vec<Value> =
        serde_json::from_slice(&inspect.stdout).map_err(|_| "fixture_inspection_invalid")?;
    if containers.len() != 1 {
        return Err("fixture_inspection_invalid");
    }
    let actual = &containers[0];
    if actual["Id"].as_str() != Some(container)
        || actual["State"]["Running"].as_bool() != Some(true)
        || actual["Config"]["Labels"]["com.completedottech.codex.pg.instance"].as_str()
            != Some(instance)
        || actual["Config"]["Labels"]["com.docker.compose.project"].as_str() != Some(project)
    {
        return Err("fixture_container_mismatch");
    }
    let bindings = actual["NetworkSettings"]["Ports"]["5432/tcp"]
        .as_array()
        .ok_or("fixture_binding_invalid")?;
    let port = receipt["port"].as_u64().ok_or("fixture_port_invalid")?;
    if !(1024..=65535).contains(&port) {
        return Err("fixture_port_invalid");
    }
    if bindings.len() != 1
        || bindings[0]["HostIp"].as_str() != Some("127.0.0.1")
        || bindings[0]["HostPort"].as_str() != Some(port.to_string().as_str())
    {
        return Err("fixture_binding_mismatch");
    }
    let password_bytes = read_file(
        &state.join(format!("secrets/{role}.password")),
        /*limit*/ 4096,
    )?;
    let password =
        std::str::from_utf8(&password_bytes).map_err(|_| "fixture_credential_invalid")?;
    Ok(ConnectionSettings {
        host: "localhost".to_string(),
        port: port as u16,
        database: "codex".to_string(),
        username: format!("codex_{role}"),
        password: password.trim().to_string().into(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(/*secs*/ 5),
            acquire_timeout: Duration::from_secs(/*secs*/ 5),
            max_connections: 3,
        },
    })
}

async fn docker_identity(
    context: &str,
    args: &[&str],
) -> Result<std::process::Output, &'static str> {
    let mut command = tokio::process::Command::new("docker");
    command
        .args(["--context", context])
        .args(args)
        .kill_on_drop(true);
    capture(
        &mut command,
        Duration::from_secs(/*secs*/ 5),
        /*limit*/ 1_048_575,
    )
    .await
}

#[cfg(test)]
#[path = "exclusive_fixture_settings_tests.rs"]
mod tests;

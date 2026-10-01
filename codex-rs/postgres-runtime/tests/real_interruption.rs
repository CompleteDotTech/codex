#![expect(
    clippy::expect_used,
    reason = "isolated PostgreSQL fixture failures should identify their source"
)]

use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolError;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::TransactionError;
use serde_json::Value;
use std::io;
use std::net::Shutdown;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::time::timeout;

fn settings(state: &Path) -> ConnectionSettings {
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt");
    ConnectionSettings {
        host: "localhost".to_string(),
        port: receipt["port"].as_u64().expect("PostgreSQL port") as u16,
        database: "codex".to_string(),
        username: "codex_runtime".to_string(),
        password: std::fs::read_to_string(state.join("secrets/runtime.password"))
            .expect("read private runtime credential")
            .trim()
            .to_string(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(2),
            max_connections: 1,
        },
    }
}

async fn manage(python: &Path, state: &Path, manager: &Path, action: &'static str) -> bool {
    let python = python.to_path_buf();
    let state = state.to_path_buf();
    let manager = manager.to_path_buf();
    tokio::task::spawn_blocking(move || {
        Command::new(python)
            .arg(manager)
            .arg("--state")
            .arg(state)
            .arg(action)
            .output()
            .is_ok_and(|result| result.status.success())
    })
    .await
    .unwrap_or(false)
}

async fn wait_until_active(pool: &PostgresPool, pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(mut connection) = pool.acquire().await {
            let state: Result<Option<String>, _> =
                sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
                    .bind(pid)
                    .fetch_optional(&mut *connection)
                    .await;
            if matches!(state.as_ref(), Ok(Some(value)) if value == "active") {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

struct LoopbackProxy {
    port: u16,
    stopping: Arc<AtomicBool>,
    sockets: Arc<Mutex<Option<(TcpStream, TcpStream)>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LoopbackProxy {
    fn start(server_port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stopping = Arc::new(AtomicBool::new(false));
        let sockets = Arc::new(Mutex::new(None));
        let thread_stopping = Arc::clone(&stopping);
        let thread_sockets = Arc::clone(&sockets);
        let worker = thread::spawn(move || {
            while !thread_stopping.load(Ordering::SeqCst) {
                let client = match listener.accept() {
                    Ok((client, _)) => client,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => return,
                };
                let server = TcpStream::connect_timeout(
                    &format!("127.0.0.1:{server_port}")
                        .parse()
                        .expect("server address"),
                    Duration::from_secs(5),
                )
                .expect("connect local PostgreSQL fixture");
                let mut client_reader = client.try_clone().expect("clone client reader");
                let mut server_writer = server.try_clone().expect("clone server writer");
                let mut server_reader = server.try_clone().expect("clone server reader");
                let mut client_writer = client.try_clone().expect("clone client writer");
                *thread_sockets.lock().expect("proxy socket lock") = Some((client, server));
                let upstream = thread::spawn(move || {
                    let _ = io::copy(&mut client_reader, &mut server_writer);
                });
                let downstream = thread::spawn(move || {
                    let _ = io::copy(&mut server_reader, &mut client_writer);
                });
                let _ = upstream.join();
                let _ = downstream.join();
                return;
            }
        });
        Ok(Self {
            port,
            stopping,
            sockets,
            worker: Some(worker),
        })
    }

    fn cut(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some((client, server)) = self.sockets.lock().expect("proxy socket lock").as_ref() {
            let _ = client.shutdown(Shutdown::Both);
            let _ = server.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for LoopbackProxy {
    fn drop(&mut self) {
        self.cut();
    }
}

#[tokio::test]
async fn real_restart_and_loopback_network_cut_are_bounded() {
    let (Ok(state), Ok(manager)) = (
        std::env::var("CODEX_TEST_POSTGRES_INTERRUPTION_STATE"),
        std::env::var("CODEX_TEST_POSTGRES_MANAGER"),
    ) else {
        return;
    };
    let state = PathBuf::from(state);
    let manager = PathBuf::from(manager);
    // WSL may read a Windows receipt while only native Windows Python can
    // operate its ACLs and Docker Desktop context. Hosted Linux uses the same
    // path for the reader and receipt-bound manager.
    let manager_state = std::env::var_os("CODEX_TEST_POSTGRES_MANAGER_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| state.clone());
    let manager_script = std::env::var_os("CODEX_TEST_POSTGRES_MANAGER_SCRIPT")
        .map(PathBuf::from)
        .unwrap_or(manager);
    let manager_python = std::env::var_os("CODEX_TEST_POSTGRES_MANAGER_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("python3"));
    let direct = PostgresPool::connect(settings(&state))
        .await
        .expect("connect direct pool");
    let observer = PostgresPool::connect(settings(&state))
        .await
        .expect("connect observer pool");

    let mut held = direct.acquire().await.expect("acquire direct connection");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .expect("get direct backend pid");
    let mut pending = tokio::spawn(async move {
        sqlx::query("SELECT pg_sleep(30)")
            .execute(&mut *held)
            .await
            .map_err(|error| TransactionError::classify_statement(&error))
    });
    let active = wait_until_active(&observer, pid).await;
    let stopped = manage(&manager_python, &manager_state, &manager_script, "stop").await;
    let interrupted = timeout(Duration::from_secs(20), &mut pending).await;
    if interrupted.is_err() {
        pending.abort();
        let _ = pending.await;
    }
    let stopped_health = direct.health().await;
    // Recovery runs before any assertion so a failed poll or query leaves the
    // receipt-bound fixture available for inspection and normal CI cleanup.
    let restarted = manage(&manager_python, &manager_state, &manager_script, "up").await;
    assert!(restarted, "receipt-bound PostgreSQL restart must succeed");
    assert!(active, "long query must be active before server stop");
    assert!(stopped, "receipt-bound server stop must succeed");
    assert!(matches!(
        interrupted,
        Ok(Ok(Err(TransactionError::Unavailable)))
    ));
    assert!(matches!(
        stopped_health,
        Err(PoolError::Unavailable | PoolError::Timeout)
    ));
    direct.health().await.expect("existing pool reconnects");
    PostgresPool::connect(settings(&state))
        .await
        .expect("new pool connects after restart")
        .health()
        .await
        .expect("new pool is healthy");

    let mut proxy = LoopbackProxy::start(settings(&state).port).expect("bind loopback proxy");
    let mut proxy_settings = settings(&state);
    proxy_settings.port = proxy.port;
    let proxied = PostgresPool::connect(proxy_settings)
        .await
        .expect("TLS connection through loopback proxy");
    let mut proxied_connection = proxied.acquire().await.expect("acquire proxied connection");
    let proxy_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *proxied_connection)
        .await
        .expect("get proxied backend pid");
    let mut pending = tokio::spawn(async move {
        sqlx::query("SELECT pg_sleep(30)")
            .execute(&mut *proxied_connection)
            .await
            .map_err(|error| TransactionError::classify_statement(&error))
    });
    let active = wait_until_active(&observer, proxy_pid).await;
    proxy.cut();
    let interrupted = timeout(Duration::from_secs(5), &mut pending).await;
    if interrupted.is_err() {
        pending.abort();
        let _ = pending.await;
    }
    assert!(active, "proxied query must be active before network cut");
    assert!(matches!(
        interrupted,
        Ok(Ok(Err(TransactionError::Unavailable)))
    ));
    direct
        .health()
        .await
        .expect("direct pool survives proxy cut");
}

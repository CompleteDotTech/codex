use super::JobObject;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::Mutex;
use tokio::process::Child;
use tokio::process::Command;
use winapi::shared::ntdef::NT_SUCCESS;
use winapi::shared::ntdef::NTSTATUS;
use winapi::um::jobapi2::QueryInformationJobObject;
use winapi::um::winnt::HANDLE;
use winapi::um::winnt::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION;
use winapi::um::winnt::JobObjectBasicAccountingInformation;

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process_handle: HANDLE) -> NTSTATUS;
}

struct Admission {
    closed: bool,
    // Failed assignment can leave a suspended root outside the job. Retain its
    // exact native owner until wait confirms exit, independently of job members.
    pending_launch: Option<Child>,
}

/// A private worker's retained, non-breakaway process-tree owner.
///
/// This deliberately exposes neither the native handle nor descendant preservation.
/// Default interactive jobs retain their existing compatibility policy.
pub struct ContainedWorkerJob {
    job: JobObject,
    admission: Mutex<Admission>,
}

/// Native zero-member evidence for an admission-closed retained worker job.
///
/// This does not prove output drain, thread shutdown, or termination of a process
/// launched outside this owner. Consumers must additionally establish those fences.
pub struct WorkerJobTerminal<'a> {
    _owner: &'a ContainedWorkerJob,
}

impl ContainedWorkerJob {
    pub fn create() -> io::Result<Self> {
        Ok(Self {
            job: JobObject::create_without_breakaway()?,
            admission: Mutex::new(Admission {
                closed: false,
                pending_launch: None,
            }),
        })
    }

    /// Assigns the captured child handle while suspended, then resumes it.
    /// Assignment failure never retries uncontained or resumes the child.
    pub fn spawn(&self, command: &mut Command) -> io::Result<Child> {
        let mut admission = self
            .admission
            .lock()
            .map_err(|_| io::Error::other("private worker job admission lock poisoned"))?;
        if admission.closed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private worker job admission is closed",
            ));
        }
        self.job.prepare_suspended_spawn(command);
        let child = command.spawn()?;
        admission.pending_launch = Some(child);
        let launched = (|| {
            let child = admission
                .pending_launch
                .as_ref()
                .ok_or_else(|| io::Error::other("missing retained suspended child"))?;
            let handle = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("missing child process handle"))?;
            self.job.assign_process(handle)?;
            let status = unsafe { NtResumeProcess(handle.cast()) };
            if !NT_SUCCESS(status) {
                return Err(io::Error::other(format!(
                    "failed to resume owned worker child: NTSTATUS {status:#x}"
                )));
            }
            Ok(())
        })();
        if let Err(error) = launched {
            admission.closed = true;
            // Neither drop nor a kill request supplies native terminal evidence.
            // The caller retains this owner even when the launch returned Err.
            return Err(error);
        }
        admission
            .pending_launch
            .take()
            .ok_or_else(|| io::Error::other("missing retained resumed child"))
    }

    /// Permanently closes admission before requesting native tree termination.
    /// A successful request alone is never terminal evidence.
    pub fn request_shutdown(&self) -> io::Result<()> {
        let mut admission = self
            .admission
            .lock()
            .map_err(|_| io::Error::other("private worker job admission lock poisoned"))?;
        admission.closed = true;
        let root_request = match admission.pending_launch.as_mut() {
            Some(child) => child.start_kill(),
            None => Ok(()),
        };
        // Attempt both owned cleanup requests even if the unassigned root fails.
        let job_request = self.job.terminate();
        root_request.and(job_request)
    }

    /// Permanently closes admission and observes the retained native job.
    /// On a live member or query error, callers must retain this owner and retry.
    pub fn observe_terminal(&self) -> io::Result<Option<WorkerJobTerminal<'_>>> {
        let mut admission = self
            .admission
            .lock()
            .map_err(|_| io::Error::other("private worker job admission lock poisoned"))?;
        admission.closed = true;
        if let Some(child) = admission.pending_launch.as_mut() {
            if child.try_wait()?.is_none() {
                return Ok(None);
            }
            admission.pending_launch.take();
        }
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let queried = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle().cast(),
                JobObjectBasicAccountingInformation,
                std::ptr::addr_of_mut!(accounting).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((accounting.ActiveProcesses == 0).then_some(WorkerJobTerminal { _owner: self }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::time::Duration;

    struct UncertainRoot {
        _owner: Arc<ContainedWorkerJob>,
        _root: Child,
    }

    static UNCERTAIN: OnceLock<Mutex<Vec<Arc<ContainedWorkerJob>>>> = OnceLock::new();

    async fn finish_native(
        owner: Arc<ContainedWorkerJob>,
        original: anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let request = owner.request_shutdown();
        let terminal = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if owner.observe_terminal()?.is_some() {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        eprintln!(
            "WORKER_FOUNDATION_CLEANUP original={original:?} request={request:?} terminal={terminal:?}"
        );
        if !matches!(terminal, Ok(Ok(()))) {
            UNCERTAIN
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(owner);
            eprintln!(
                "WORKER_FOUNDATION_UNCERTAIN_HOST_RETAINED: supervisor reconciliation required"
            );
            return std::future::pending::<anyhow::Result<()>>().await;
        }
        original?;
        request?;
        Ok(())
    }
    #[tokio::test]
    #[ignore = "requires explicitly admitted native Windows child-process fixture"]
    async fn assignment_failure_retains_unassigned_suspended_child() -> anyhow::Result<()> {
        anyhow::ensure!(
            std::env::var("CODEX_TEST_CONTAINED_WORKER_NATIVE").as_deref() == Ok("1"),
            "selected native fixture requires explicit admission",
        );
        let owner = Arc::new(ContainedWorkerJob::create()?);
        let original =
            async {
                let mut limits: winapi::um::winnt::JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
                    unsafe { std::mem::zeroed() };
                limits.BasicLimitInformation.LimitFlags =
                    winapi::um::winnt::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                        | winapi::um::winnt::JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                limits.BasicLimitInformation.ActiveProcessLimit = 1;
                let configured = unsafe {
                    winapi::um::jobapi2::SetInformationJobObject(
                        owner.job.as_raw_handle().cast(),
                        winapi::um::winnt::JobObjectExtendedLimitInformation,
                        std::ptr::addr_of_mut!(limits).cast(),
                        std::mem::size_of_val(&limits) as u32,
                    )
                };
                anyhow::ensure!(configured != 0, "{}", io::Error::last_os_error());
                let mut command = Command::new(std::env::current_exe()?);
                command.args(["--ignored", "--exact",
            "win::contained_worker::tests::root_exit_does_not_acknowledge_live_descendant"])
            .env("CODEX_TEST_CONTAINED_WORKER_PHASE", "descendant")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                let mut admitted = owner.spawn(&mut command)?;
                anyhow::ensure!(owner.spawn(&mut command).is_err());
                {
                    let admission = owner
                        .admission
                        .lock()
                        .map_err(|_| io::Error::other("poisoned"))?;
                    anyhow::ensure!(admission.closed);
                    anyhow::ensure!(
                        admission
                            .pending_launch
                            .as_ref()
                            .and_then(Child::id)
                            .is_some(),
                        "failed native assignment lost the exact suspended child"
                    );
                }
                anyhow::ensure!(owner.observe_terminal()?.is_none());
                owner.request_shutdown()?;
                tokio::time::timeout(Duration::from_secs(/*secs*/ 5), admitted.wait()).await??;
                tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
                    loop {
                        if owner.observe_terminal()?.is_some() {
                            anyhow::ensure!(
                                owner
                                    .admission
                                    .lock()
                                    .map_err(|_| io::Error::other("poisoned"))?
                                    .pending_launch
                                    .is_none()
                            );
                            return Ok::<(), anyhow::Error>(());
                        }
                        tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
                    }
                })
                .await??;
                Ok(())
            }
            .await;
        finish_native(owner, original).await
    }

    #[tokio::test]
    #[ignore = "requires explicitly admitted native Windows child-process fixture"]
    async fn root_exit_does_not_acknowledge_live_descendant() -> anyhow::Result<()> {
        const NAME: &str =
            "win::contained_worker::tests::root_exit_does_not_acknowledge_live_descendant";
        const PHASE: &str = "CODEX_TEST_CONTAINED_WORKER_PHASE";
        let executable = std::env::current_exe()?;
        match std::env::var(PHASE).as_deref() {
            Ok("descendant") => {
                std::thread::sleep(Duration::from_secs(/*secs*/ 30));
                return Ok(());
            }
            Ok("root") => {
                let mut breakaway = std::process::Command::new(&executable);
                breakaway
                    .args(["--ignored", "--exact", NAME])
                    .env(PHASE, "descendant")
                    .creation_flags(winapi::um::winbase::CREATE_BREAKAWAY_FROM_JOB);
                if let Ok(mut escaped) = breakaway.spawn() {
                    // Retain and clean the exact unexpected child before failing.
                    let killed = escaped.kill();
                    let terminal = escaped.try_wait();
                    if !matches!(&terminal, Ok(Some(_))) {
                        static ESCAPED: OnceLock<Mutex<Vec<std::process::Child>>> = OnceLock::new();
                        ESCAPED
                            .get_or_init(|| Mutex::new(Vec::new()))
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(escaped);
                        eprintln!(
                            "UNEXPECTED_BREAKAWAY_OWNER_RETAINED killed={killed:?} terminal={terminal:?}; supervisor must reconcile same host"
                        );
                        return std::future::pending::<anyhow::Result<()>>().await;
                    }
                    anyhow::bail!("owned child escaped its job");
                }
                let _descendant = std::process::Command::new(&executable)
                    .args(["--ignored", "--exact", NAME])
                    .env(PHASE, "descendant")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
                return Ok(());
            }
            _ => {}
        }
        anyhow::ensure!(
            std::env::var("CODEX_TEST_CONTAINED_WORKER_NATIVE").as_deref() == Ok("1"),
            "selected native fixture requires explicit admission",
        );
        let owner = Arc::new(ContainedWorkerJob::create()?);
        let original = async {
            let mut command = Command::new(executable);
            command
                .args(["--ignored", "--exact", NAME])
                .env(PHASE, "root")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut root = owner.spawn(&mut command)?;
            let root_status = match tokio::time::timeout(Duration::from_secs(5), root.wait()).await {
                Ok(Ok(status)) => status,
                uncertain => {
                    // A root may itself retain an unexpected outside-job child.
                    // Never kill its host before an actual root terminal observation.
                    static ROOTS: OnceLock<Mutex<Vec<UncertainRoot>>> = OnceLock::new();
                    ROOTS.get_or_init(|| Mutex::new(Vec::new())).lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(UncertainRoot { _owner: Arc::clone(&owner), _root: root });
                    eprintln!("WORKER_FOUNDATION_ROOT_UNCERTAIN_RETAINED {uncertain:?}; no shutdown request; supervisor must reconcile exact host");
                    return std::future::pending::<anyhow::Result<()>>().await;
                }
            };
            anyhow::ensure!(root_status.success());
            anyhow::ensure!(
                owner.observe_terminal()?.is_none(),
                "root exit falsely acknowledged descendant"
            );
            anyhow::ensure!(
                owner.spawn(&mut command).is_err(),
                "terminal observation reopened admission"
            );
            owner.request_shutdown()?;
            tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
                loop {
                    if owner.observe_terminal()?.is_some() {
                        return Ok::<(), anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
                }
            })
            .await??;
            Ok(())
        }
        .await;
        finish_native(owner, original).await
    }
}

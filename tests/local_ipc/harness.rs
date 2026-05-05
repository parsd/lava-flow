use crate::IpcCase;
use lava_flow::types::ChannelId;
use std::env;
use std::error::Error;
use std::io;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHILD_ROLE_ENV: &str = "LAVA_FLOW_IPC_TEST_ROLE";
const CASE_ENV: &str = "LAVA_FLOW_IPC_TEST_CASE";
const CHANNEL_ENV: &str = "LAVA_FLOW_IPC_TEST_CHANNEL";
const READY_FILE_ENV: &str = "LAVA_FLOW_IPC_TEST_READY_FILE";
const SIZE_ENV: &str = "LAVA_FLOW_IPC_TEST_SIZE";
const SEED_ENV: &str = "LAVA_FLOW_IPC_TEST_SEED";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const BUILD_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct ChildConfig {
    pub(crate) role: String,
    pub(crate) case: String,
    pub(crate) channel_id: ChannelId,
    pub(crate) size: usize,
    pub(crate) seed: u8,
}

impl ChildConfig {
    pub(crate) fn new() -> Result<Option<Self>, Box<dyn Error>> {
        let Ok(role) = env::var(CHILD_ROLE_ENV) else {
            return Ok(None);
        };
        Ok(Some(Self {
            role,
            case: required_env(CASE_ENV)?,
            channel_id: ChannelId::new(required_env(CHANNEL_ENV)?)?,
            size: required_env(SIZE_ENV)?.parse::<usize>()?,
            seed: required_env(SEED_ENV)?.parse::<u8>()?,
        }))
    }
}

pub(crate) fn run_interprocess_case(
    case: IpcCase,
    size: usize,
    seed: u8,
) -> Result<(), Box<dyn Error>> {
    let id = unique_id(case);
    let channel_id = format!("local-ipc-{id}");
    let runtime_dir = RuntimeDir::create(&id)?;
    let ready_file = ReadyFile::new(&id);
    let mut sender = spawn_child(
        "sender",
        case,
        &channel_id,
        size,
        seed,
        runtime_dir.path(),
        Some(ready_file.path()),
    )?;
    if let Err(source) = ready_file.wait_for_signal(&mut sender, BUILD_TIMEOUT) {
        let _ = sender.kill();
        let _ = sender.wait();
        return Err(source.into());
    }
    let receiver = match spawn_child(
        "receiver",
        case,
        &channel_id,
        size,
        seed,
        runtime_dir.path(),
        None,
    ) {
        Ok(receiver) => receiver,
        Err(source) => {
            let _ = sender.kill();
            let _ = sender.wait();
            return Err(source.into());
        }
    };

    let receiver = wait_with_timeout(receiver, PROCESS_TIMEOUT)?;
    let sender = wait_with_timeout(sender, PROCESS_TIMEOUT)?;

    assert_success("receiver", &receiver);
    assert_success("sender", &sender);
    Ok(())
}

fn spawn_child(
    role: &str,
    case: IpcCase,
    channel_id: &str,
    size: usize,
    seed: u8,
    runtime_dir: Option<&std::path::Path>,
    ready_file: Option<&std::path::Path>,
) -> io::Result<Child> {
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--exact")
        .arg("local_ipc_child_entry")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(CHILD_ROLE_ENV, role)
        .env(CASE_ENV, case.as_str())
        .env(CHANNEL_ENV, channel_id)
        .env(SIZE_ENV, size.to_string())
        .env(SEED_ENV, seed.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(runtime_dir) = runtime_dir {
        command.env("LAVA_FLOW_RUNTIME_DIR", runtime_dir);
    }
    if let Some(ready_file) = ready_file {
        command.env(READY_FILE_ENV, ready_file);
    }

    command.spawn()
}

fn wait_with_timeout(mut child: Child, timeout: Duration) -> io::Result<Output> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = child.kill();
    child.wait_with_output()
}

fn assert_success(label: &str, output: &Output) {
    if !output.status.success() {
        panic!(
            "{label} failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn required_env(name: &str) -> Result<String, Box<dyn Error>> {
    env::var(name).map_err(|source| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing required env {name}: {source}"),
        )
        .into()
    })
}

fn unique_id(case: IpcCase) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    format!("{}-{}-{counter}-{nanos}", case.as_str(), std::process::id())
}

struct RuntimeDir {
    path: Option<std::path::PathBuf>,
}

impl RuntimeDir {
    fn create(id: &str) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let path = env::temp_dir().join(format!("lava-flow-{id}"));
            std::fs::create_dir_all(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            Ok(Self { path: Some(path) })
        }
        #[cfg(not(unix))]
        {
            let _ = id;
            Ok(Self { path: None })
        }
    }

    fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub(crate) struct ReadyFile {
    path: std::path::PathBuf,
}

impl ReadyFile {
    fn new(id: &str) -> Self {
        let path = env::temp_dir().join(format!("lava-flow-ready-{id}"));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn wait_for_signal(&self, child: &mut Child, timeout: Duration) -> io::Result<()> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.path.exists() {
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "child exited before signaling readiness: {status}"
                )));
            }
            thread::sleep(Duration::from_millis(10));
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "timed out waiting for readiness file {}",
                self.path.display()
            ),
        ))
    }

    pub(crate) fn signal() -> io::Result<()> {
        match env::var_os(READY_FILE_ENV) {
            Some(path) => std::fs::write(path, b"ready"),
            None => Ok(()),
        }
    }
}

impl Drop for ReadyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

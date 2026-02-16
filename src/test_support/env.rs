use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Guards a temporary environment variable override during tests.
pub(crate) struct Guard {
    key: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Guard {
    /// Sets `key=value` for the guard lifetime and restores the previous state on drop.
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let lock = env_lock().lock().expect("lock env for test");
        let previous = std::env::var_os(key);
        // Serialized with a process-wide lock for test safety.
        unsafe { std::env::set_var(key, value) };
        Self {
            key,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

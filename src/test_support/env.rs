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
        let lock = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os(key);
        // Serialized with a process-wide lock for test safety.
        unsafe { std::env::set_var(key, value) };
        Self {
            key,
            previous,
            _lock: lock,
        }
    }

    /// Removes `key` for the guard lifetime and restores the previous state on drop.
    pub(crate) fn unset(key: &'static str) -> Self {
        let lock = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os(key);
        // Serialized with a process-wide lock for test safety.
        unsafe { std::env::remove_var(key) };
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_SET_RESTORE: &str = "LAVA_FLOW_TEST_ENV_GUARD_KEY_SET_RESTORE";
    const KEY_SET_ABSENT: &str = "LAVA_FLOW_TEST_ENV_GUARD_KEY_SET_ABSENT";
    const KEY_UNSET_RESTORE: &str = "LAVA_FLOW_TEST_ENV_GUARD_KEY_UNSET_RESTORE";
    const KEY_POISONED: &str = "LAVA_FLOW_TEST_ENV_GUARD_KEY_POISONED";
    const ORIGINAL: &str = "original";
    const TEMP: &str = "temporary";

    #[test]
    fn guard_restores_previous_value() {
        unsafe { std::env::set_var(KEY_SET_RESTORE, ORIGINAL) };
        {
            let _guard = Guard::set(KEY_SET_RESTORE, TEMP);
            assert_eq!(std::env::var(KEY_SET_RESTORE).as_deref(), Ok(TEMP));
        }
        assert_eq!(std::env::var(KEY_SET_RESTORE).as_deref(), Ok(ORIGINAL));
        unsafe { std::env::remove_var(KEY_SET_RESTORE) };
    }

    #[test]
    fn guard_removes_value_when_key_was_absent() {
        unsafe { std::env::remove_var(KEY_SET_ABSENT) };
        {
            let _guard = Guard::set(KEY_SET_ABSENT, TEMP);
            assert_eq!(std::env::var(KEY_SET_ABSENT).as_deref(), Ok(TEMP));
        }
        assert!(std::env::var(KEY_SET_ABSENT).is_err());
    }

    #[test]
    fn guard_unset_restores_previous_value() {
        unsafe { std::env::set_var(KEY_UNSET_RESTORE, ORIGINAL) };
        {
            let _guard = Guard::unset(KEY_UNSET_RESTORE);
            assert!(std::env::var(KEY_UNSET_RESTORE).is_err());
        }
        assert_eq!(std::env::var(KEY_UNSET_RESTORE).as_deref(), Ok(ORIGINAL));
        unsafe { std::env::remove_var(KEY_UNSET_RESTORE) };
    }

    #[test]
    fn guard_set_recovers_from_poisoned_lock() {
        let _ = std::panic::catch_unwind(|| {
            let _lock = env_lock().lock().expect("lock env mutex");
            panic!("poison env mutex");
        });

        unsafe { std::env::remove_var(KEY_POISONED) };
        {
            let _guard = Guard::set(KEY_POISONED, TEMP);
            assert_eq!(std::env::var(KEY_POISONED).as_deref(), Ok(TEMP));
        }
        assert!(std::env::var(KEY_POISONED).is_err());
    }
}

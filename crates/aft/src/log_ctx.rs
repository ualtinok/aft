//! Thread-local session context for log lines.
//!
//! AFT runs a single-threaded request loop. Each incoming request carries a
//! `session_id` that identifies the OpenCode/Pi session. By storing it in a
//! thread-local we can automatically prepend `[ses_xxx]` to every `slog_*`
//! log macro call without threading the session id through every function
//! signature.
//!
//! Background threads spawned during request handling (search-index pre-warm,
//! semantic-index build) **must** capture the session id before spawning and
//! re-install it on the new thread via [`set_session`] or [`with_session`].

use std::cell::RefCell;

thread_local! {
    /// Current session id for log tagging. `None` means "no session context".
    static CURRENT_SESSION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Set the current thread-local session id.
///
/// Call this at the start of a background thread that captured the session id
/// from the parent request loop.
pub fn set_session(session: Option<String>) {
    CURRENT_SESSION.with(|s| {
        *s.borrow_mut() = session;
    });
}

struct SessionGuard(Option<String>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        set_session(self.0.take());
    }
}

/// Run `f` with the given session id set on the current thread, restoring the
/// previous value afterwards (RAII-style and panic-safe).
///
/// This is the primary entry point for the main request loop: wrap the
/// dispatch call in `with_session(req.session_id.clone(), || { ... })`.
pub fn with_session<T>(session: Option<String>, f: impl FnOnce() -> T) -> T {
    let prev = current_session();
    set_session(session);
    let _guard = SessionGuard(prev);
    f()
}

/// Return the current session id (e.g. `"abcd1234"`), or `None` if no session is set.
pub fn current_session() -> Option<String> {
    CURRENT_SESSION.with(|s| s.borrow().clone())
}

/// Return the current session id prefix string, e.g. `"[ses_abcd1234] "`,
/// or an empty string if no session is set.
pub fn session_prefix() -> String {
    CURRENT_SESSION.with(|s| match s.borrow().as_deref() {
        Some(sid) => format!("[ses_{}] ", sid),
        None => String::new(),
    })
}

/// Log at INFO level with the `[aft]` prefix and optional `[ses_xxx]` session tag.
///
/// Use this instead of `log::info!("[aft] ...")` in per-request code paths.
/// The macro automatically reads the thread-local session id and formats:
///
/// ```text
/// With session:    [aft] [ses_abcd1234] semantic index: rebuilding from scratch
/// Without session: [aft] semantic index: rebuilding from scratch
/// ```
#[macro_export]
macro_rules! slog_info {
    ($($arg:tt)*) => {
        log::info!("[aft] {}{}", $crate::log_ctx::session_prefix(), format!($($arg)*))
    };
}

/// Log at WARN level with the `[aft]` prefix and optional `[ses_xxx]` session tag.
///
/// See [`slog_info!`] for format details.
#[macro_export]
macro_rules! slog_warn {
    ($($arg:tt)*) => {
        log::warn!("[aft] {}{}", $crate::log_ctx::session_prefix(), format!($($arg)*))
    };
}

/// Log at ERROR level with the `[aft]` prefix and optional `[ses_xxx]` session tag.
///
/// See [`slog_info!`] for format details.
#[macro_export]
macro_rules! slog_error {
    ($($arg:tt)*) => {
        log::error!("[aft] {}{}", $crate::log_ctx::session_prefix(), format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_session_sets_and_clears() {
        // Initially no session
        CURRENT_SESSION.with(|s| {
            assert!(s.borrow().is_none());
        });

        // Set inside with_session
        with_session(Some("test123".to_string()), || {
            CURRENT_SESSION.with(|s| {
                assert_eq!(s.borrow().as_deref(), Some("test123"));
            });
        });

        // Cleared after with_session
        CURRENT_SESSION.with(|s| {
            assert!(s.borrow().is_none());
        });
    }

    #[test]
    fn with_session_none_is_noop() {
        with_session(None, || {
            CURRENT_SESSION.with(|s| {
                assert!(s.borrow().is_none());
            });
        });
    }

    #[test]
    fn session_prefix_format() {
        with_session(Some("abcd1234".to_string()), || {
            assert_eq!(session_prefix(), "[ses_abcd1234] ");
        });

        // Without session
        assert_eq!(session_prefix(), "");
    }

    #[test]
    fn set_session_direct() {
        set_session(Some("direct".to_string()));
        CURRENT_SESSION.with(|s| {
            assert_eq!(s.borrow().as_deref(), Some("direct"));
        });
        set_session(None);
        CURRENT_SESSION.with(|s| {
            assert!(s.borrow().is_none());
        });
    }
}

use crate::internal::{PanicCause, PanicLocation};
use std::backtrace::Backtrace;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Once};

thread_local! {
    static CURRENT_TEST_ID: Cell<Option<u64>> = const { Cell::new(None) };
}

static PANIC_CAPTURES: LazyLock<Mutex<HashMap<u64, Vec<PanicCause>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

type DetachedCollectors = HashMap<u64, Arc<Mutex<Vec<PanicCause>>>>;

static DETACHED_COLLECTORS: LazyLock<Mutex<DetachedCollectors>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static INSTALL_HOOK: Once = Once::new();

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_test_id() -> u64 {
    NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn set_current_test_id(id: u64) {
    CURRENT_TEST_ID.set(Some(id));
}

pub(crate) fn clear_current_test_id() {
    CURRENT_TEST_ID.set(None);
}

fn lock_captures() -> std::sync::MutexGuard<'static, HashMap<u64, Vec<PanicCause>>> {
    match PANIC_CAPTURES.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(crate) fn take_panic_capture(id: u64) -> Option<PanicCause> {
    let mut guard = lock_captures();
    let vec = guard.get_mut(&id)?;
    let cause = vec.pop();
    if vec.is_empty() {
        guard.remove(&id);
    }
    cause
}

pub(crate) fn take_current_panic_capture() -> Option<PanicCause> {
    CURRENT_TEST_ID.get().and_then(take_panic_capture)
}

pub(crate) fn current_test_id() -> Option<u64> {
    CURRENT_TEST_ID.get()
}

pub(crate) fn create_detached_collector(test_id: u64) -> Arc<Mutex<Vec<PanicCause>>> {
    let collector = Arc::new(Mutex::new(Vec::new()));
    lock_detached_collectors().insert(test_id, collector.clone());
    collector
}

pub(crate) fn take_detached_collector(test_id: u64) -> Option<Arc<Mutex<Vec<PanicCause>>>> {
    lock_detached_collectors().remove(&test_id)
}

pub(crate) fn get_detached_collector(test_id: u64) -> Option<Arc<Mutex<Vec<PanicCause>>>> {
    lock_detached_collectors().get(&test_id).cloned()
}

fn lock_detached_collectors(
) -> std::sync::MutexGuard<'static, HashMap<u64, Arc<Mutex<Vec<PanicCause>>>>> {
    match DETACHED_COLLECTORS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(crate) fn install_panic_hook() {
    INSTALL_HOOK.call_once(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Use try_with to avoid panicking during thread teardown
            let test_id = CURRENT_TEST_ID.try_with(|c| c.get()).ok().flatten();
            if let Some(id) = test_id {
                let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
                    Some(s.to_string())
                } else {
                    info.payload().downcast_ref::<String>().cloned()
                };

                let location = info.location().map(|loc| PanicLocation {
                    file: loc.file().to_string(),
                    line: loc.line(),
                    column: loc.column(),
                });

                let backtrace = Arc::new(Backtrace::capture());

                let cause = PanicCause {
                    message,
                    location,
                    backtrace: Some(backtrace),
                };

                // Best-effort capture: skip if lock is contended to avoid deadlock in hook
                if let Ok(mut guard) = PANIC_CAPTURES.try_lock() {
                    guard.entry(id).or_default().push(cause);
                }
            } else {
                previous_hook(info);
            }
        }));
    });
}

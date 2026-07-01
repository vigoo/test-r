use crate::args::{Arguments, TimeThreshold};
use crate::bench::Bencher;
use crate::stats::Summary;
use std::any::{Any, TypeId};
use std::backtrace::Backtrace;
use std::cmp::{max, Ordering};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub enum TestFunction {
    Sync(
        Arc<
            dyn Fn(Arc<dyn DependencyView + Send + Sync>) -> Box<dyn TestReturnValue>
                + Send
                + Sync
                + 'static,
        >,
    ),
    SyncBench(
        Arc<dyn Fn(&mut Bencher, Arc<dyn DependencyView + Send + Sync>) + Send + Sync + 'static>,
    ),
    #[cfg(feature = "tokio")]
    Async(
        Arc<
            dyn (Fn(
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = Box<dyn TestReturnValue>>>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
    #[cfg(feature = "tokio")]
    AsyncBench(
        Arc<
            dyn for<'a> Fn(
                    &'a mut crate::bench::AsyncBencher,
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = ()> + 'a>>
                + Send
                + Sync
                + 'static,
        >,
    ),
}

impl TestFunction {
    #[cfg(not(feature = "tokio"))]
    pub fn is_bench(&self) -> bool {
        matches!(self, TestFunction::SyncBench(_))
    }

    #[cfg(feature = "tokio")]
    pub fn is_bench(&self) -> bool {
        matches!(
            self,
            TestFunction::SyncBench(_) | TestFunction::AsyncBench(_)
        )
    }
}

pub trait TestReturnValue {
    fn into_result(self: Box<Self>) -> Result<(), FailureCause>;
}

impl TestReturnValue for () {
    fn into_result(self: Box<Self>) -> Result<(), FailureCause> {
        Ok(())
    }
}

impl<T, E: Display + Debug + Send + Sync + 'static> TestReturnValue for Result<T, E> {
    fn into_result(self: Box<Self>) -> Result<(), FailureCause> {
        match *self {
            Ok(_) => Ok(()),
            Err(e) => Err(FailureCause::from_error(e)),
        }
    }
}

#[derive(Clone)]
pub enum FailureCause {
    /// Test returned Err(e) where E: Display + Debug — stores both representations
    /// and the original error value for later downcasting
    ReturnedError {
        display: String,
        debug: String,
        prefer_debug: bool,
        error: Arc<dyn Any + Send + Sync>,
    },
    /// Test returned Err(String) — stored as raw string without formatting
    ReturnedMessage(String),
    /// Test panicked
    Panic(PanicCause),
    /// Framework error (join failure, timeout, IPC deserialization, etc.)
    HarnessError(String),
}

#[derive(Debug, Clone)]
pub struct PanicCause {
    pub message: Option<String>,
    pub location: Option<PanicLocation>,
    pub backtrace: Option<Arc<Backtrace>>,
}

#[derive(Debug, Clone)]
pub struct PanicLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

impl std::fmt::Debug for FailureCause {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureCause::ReturnedError { display, .. } => {
                f.debug_tuple("ReturnedError").field(display).finish()
            }
            FailureCause::ReturnedMessage(s) => f.debug_tuple("ReturnedMessage").field(s).finish(),
            FailureCause::Panic(p) => f.debug_tuple("Panic").field(p).finish(),
            FailureCause::HarnessError(s) => f.debug_tuple("HarnessError").field(s).finish(),
        }
    }
}

impl FailureCause {
    pub fn from_error<E: Display + Debug + Send + Sync + 'static>(e: E) -> Self {
        if TypeId::of::<E>() == TypeId::of::<String>() {
            let any: Box<dyn Any + Send + Sync> = Box::new(e);
            return FailureCause::ReturnedMessage(*any.downcast::<String>().unwrap());
        }

        let mut _prefer_debug = false;
        #[cfg(feature = "anyhow")]
        {
            _prefer_debug = TypeId::of::<E>() == TypeId::of::<anyhow::Error>();
        }

        FailureCause::ReturnedError {
            display: format!("{e:#}"),
            debug: format!("{e:?}"),
            prefer_debug: _prefer_debug,
            error: Arc::new(e),
        }
    }

    pub fn render(&self) -> String {
        match self {
            FailureCause::ReturnedError {
                display,
                debug,
                prefer_debug,
                ..
            } => {
                if *prefer_debug {
                    debug.clone()
                } else {
                    display.clone()
                }
            }
            FailureCause::ReturnedMessage(s) => s.clone(),
            FailureCause::Panic(p) => p.render(),
            FailureCause::HarnessError(s) => s.clone(),
        }
    }

    /// Get the message string for ShouldPanic matching (without backtrace)
    pub fn panic_message(&self) -> Option<&str> {
        match self {
            FailureCause::Panic(p) => p.message.as_deref(),
            _ => None,
        }
    }
}

impl PanicCause {
    pub fn render(&self) -> String {
        let mut out = self.message.clone().unwrap_or_default();
        if let Some(loc) = &self.location {
            out.push_str(&format!("\n  at {}:{}:{}", loc.file, loc.line, loc.column));
        }
        if let Some(bt) = &self.backtrace {
            let bt_str = format!("{bt}");
            if !bt_str.is_empty() && bt_str != "disabled backtrace" {
                out.push_str(&format!("\n\nStack backtrace:\n{bt}"));
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShouldPanic {
    No,
    Yes,
    WithMessage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestType {
    UnitTest,
    IntegrationTest,
}

impl TestType {
    pub fn from_path(path: &str) -> Self {
        if path.contains("/src/") {
            TestType::UnitTest
        } else {
            TestType::IntegrationTest
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlakinessControl {
    None,
    ProveNonFlaky(usize),
    RetryKnownFlaky(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachedPanicPolicy {
    FailTest,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureControl {
    Default,
    AlwaysCapture,
    NeverCapture,
}

impl CaptureControl {
    pub fn requires_capturing(&self, default: bool) -> bool {
        match self {
            CaptureControl::Default => default,
            CaptureControl::AlwaysCapture => true,
            CaptureControl::NeverCapture => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportTimeControl {
    Default,
    Enabled,
    Disabled,
}

#[derive(Clone)]
pub struct TestProperties {
    pub should_panic: ShouldPanic,
    pub test_type: TestType,
    pub timeout: Option<Duration>,
    pub flakiness_control: FlakinessControl,
    pub capture_control: CaptureControl,
    pub report_time_control: ReportTimeControl,
    pub ensure_time_control: ReportTimeControl,
    pub tags: Vec<String>,
    pub is_ignored: bool,
    pub detached_panic_policy: DetachedPanicPolicy,
}

impl TestProperties {
    pub fn unit_test() -> Self {
        TestProperties {
            test_type: TestType::UnitTest,
            ..Default::default()
        }
    }

    pub fn integration_test() -> Self {
        TestProperties {
            test_type: TestType::IntegrationTest,
            ..Default::default()
        }
    }
}

impl Default for TestProperties {
    fn default() -> Self {
        Self {
            should_panic: ShouldPanic::No,
            test_type: TestType::UnitTest,
            timeout: None,
            flakiness_control: FlakinessControl::None,
            capture_control: CaptureControl::Default,
            report_time_control: ReportTimeControl::Default,
            ensure_time_control: ReportTimeControl::Default,
            tags: Vec::new(),
            is_ignored: false,
            detached_panic_policy: DetachedPanicPolicy::FailTest,
        }
    }
}

#[derive(Clone)]
pub struct RegisteredTest {
    pub name: String,
    pub crate_name: String,
    pub module_path: String,
    pub run: TestFunction,
    pub props: TestProperties,
    pub dependencies: Option<Vec<String>>,
}

impl RegisteredTest {
    pub fn filterable_name(&self) -> String {
        if !self.module_path.is_empty() {
            format!("{}::{}", self.module_path, self.name)
        } else {
            self.name.clone()
        }
    }

    pub fn fully_qualified_name(&self) -> String {
        [&self.crate_name, &self.module_path, &self.name]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }

    pub fn crate_and_module(&self) -> String {
        [&self.crate_name, &self.module_path]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

impl Debug for RegisteredTest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredTest")
            .field("name", &self.name)
            .field("crate_name", &self.crate_name)
            .field("module_path", &self.module_path)
            .finish()
    }
}

pub static REGISTERED_TESTS: Mutex<Vec<RegisteredTest>> = Mutex::new(Vec::new());

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub enum DependencyConstructor {
    Sync(
        Arc<
            dyn (Fn(Arc<dyn DependencyView + Send + Sync>) -> Arc<dyn Any + Send + Sync + 'static>)
                + Send
                + Sync
                + 'static,
        >,
    ),
    Async(
        Arc<
            dyn (Fn(
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = Arc<dyn Any + Send + Sync>>>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

/// User-facing trait that opts a dependency value into the `Cloneable`
/// sharing strategy. The parent calls [`to_wire`](CloneableDep::to_wire) once
/// and ships the bytes to each worker via IPC. Each worker calls
/// [`from_wire`](CloneableDep::from_wire) to reconstruct a local value.
///
/// The on-the-wire encoding is entirely up to the implementor: `serde_json`,
/// `bincode`, `postcard`, a hand-rolled binary format, an on-disk file path,
/// etc. The bytes are treated as opaque by the runner.
///
/// The simple `Self`-returning `from_wire` covers Cloneable deps that need no
/// other worker-local context. If reconstruction needs worker-local state (for
/// example, a per-worker engine), model that state as a separate dependency and
/// combine the two from the test or a higher-level helper.
pub trait CloneableDep: Sized + Send + Sync + 'static {
    /// Serialise this value into wire bytes for transmission to workers.
    fn to_wire(&self) -> Vec<u8>;

    /// Reconstruct a value from wire bytes received from the parent.
    fn from_wire(bytes: &[u8]) -> Self;
}

/// User-facing trait that opts a dependency value into the `Hosted`
/// sharing strategy. Like [`CloneableDep`], but the owner instance lives in
/// the **parent test runner process** for the entire suite. The parent runs
/// the constructor once per Hosted dep and keeps the value alive until every
/// worker has finished — useful for singleton services like an in-process
/// TCP listener, a Docker container, an env-based test environment, or any
/// long-running runtime that must not be duplicated across worker processes.
///
/// The parent calls [`descriptor`](HostedDep::descriptor) on its owner once
/// and forwards the resulting bytes to every worker over IPC. Each worker
/// reconstructs a local handle via
/// [`from_descriptor`](HostedDep::from_descriptor) — the handle typically
/// connects to the live owner held by the parent (e.g. opens a TCP
/// connection to the address carried in the descriptor).
///
/// For descriptor-based Hosted deps, the owner and worker handle share the same
/// type `Self`. The implementation is responsible for stashing owner-only
/// state (sockets, background threads, etc.) in fields that the worker side
/// won't touch.
pub trait HostedDep: Sized + Send + Sync + 'static {
    /// Owner-side: produce the descriptor bytes that workers will use to
    /// reconstruct a connected handle.
    fn descriptor(&self) -> Vec<u8>;

    /// Worker-side: reconstruct a handle from descriptor bytes received from
    /// the parent.
    fn from_descriptor(bytes: &[u8]) -> Self;
}

/// Async counterpart of [`HostedDep`]. Implement this when worker-side
/// reconstruction needs to `.await` (e.g. opening async network
/// clients, doing async filesystem work, calling
/// `Provided*::new(...).await` constructors).
///
/// No opt-in flag is required: the helper functions emitted by
/// `#[test_dep(scope = Hosted)]` auto-select the async path under the `tokio`
/// runtime, so simply implementing `AsyncHostedDep` is enough.
///
/// ```ignore
/// #[test_dep(scope = Hosted)]
/// async fn dependencies() -> EnvBasedTestDependencies { /* … */ }
///
/// impl test_r::core::AsyncHostedDep for EnvBasedTestDependencies {
///     fn descriptor(&self) -> Vec<u8> { /* … */ }
///
///     async fn from_descriptor(bytes: &[u8]) -> Self { /* … */ }
/// }
/// ```
///
/// `AsyncHostedDep` is the tokio-only async counterpart of
/// [`HostedDep`]. Under the `tokio` test runtime the Hosted helper
/// functions emitted by `#[test_dep(scope = Hosted)]` always go
/// through `AsyncHostedDep`, regardless of whether the user wrote
/// `impl HostedDep` (covered by the blanket bridge below) or
/// `impl AsyncHostedDep` directly. Under the sync runtime, the same
/// helpers stay on `HostedDep`: a `scope = Hosted` registration that
/// uses an async-only `AsyncHostedDep` type therefore fails to
/// compile in sync builds. (Writing the impl on its own still compiles fine;
/// only the `#[test_dep(scope = Hosted)]` registration fails.)
///
/// `descriptor()` stays synchronous and is called on the parent owner
/// value, exactly as in [`HostedDep`]; the only difference is that
/// `from_descriptor` returns a `Future` so worker-side reconstruction
/// can await.
///
/// The legacy `async_worker` attribute is **deprecated** and ignored
/// (it now only triggers a compile-time deprecation warning at the
/// dep's registration site); remove it from any new code.
pub trait AsyncHostedDep: Sized + Send + Sync + 'static {
    /// Owner-side: produce the descriptor bytes that workers will use to
    /// reconstruct a connected handle. Called from the parent runner
    /// process exactly once per Hosted dep, just like
    /// [`HostedDep::descriptor`].
    fn descriptor(&self) -> Vec<u8>;

    /// Worker-side: asynchronously reconstruct a handle from descriptor
    /// bytes received from the parent.
    fn from_descriptor(bytes: &[u8]) -> impl std::future::Future<Output = Self> + Send;
}

/// Blanket bridge: every [`HostedDep`] is automatically also an
/// [`AsyncHostedDep`]. The bridged `from_descriptor` returns
/// [`std::future::ready`], so the bridge itself adds only an immediately-ready
/// future and the runtime can await all Hosted reconstruction uniformly.
///
/// This lets the test-r runtime drive **every** Hosted descriptor
/// reconstruction through one async path under the `tokio` runtime, regardless
/// of whether the dep's own implementation is sync (`impl HostedDep for ...`)
/// or async (`impl AsyncHostedDep for ...`). The `async_worker` macro flag is
/// unnecessary — the implementor picks sync vs async purely at the trait-impl
/// call site.
///
/// **Cost:** the bridge itself is negligible (`ready(x).await` is an
/// immediately-ready future); when the runtime later routes every
/// Hosted reconstruction through the async path there is still a
/// small async-wrapper overhead per worker-startup-per-dep (polling
/// the outer future, and possibly one boxed-future allocation from
/// `WorkerReconstructor::Async`), but no extra user-level async work.
///
/// **Coherence note:** on stable Rust, with this blanket impl in place
/// a concrete type cannot manually implement both `HostedDep` and
/// `AsyncHostedDep` — rustc rejects that as conflicting
/// implementations. That compile-time error is the intended signal
/// that one of the two manual impls is redundant and should be
/// removed.
///
/// **Source-compat note:** if downstream code imports both
/// `HostedDep` and `AsyncHostedDep` into the same scope and calls
/// trait methods by method syntax (e.g.
/// `MyType::from_descriptor(bytes)` or `dep.descriptor()`), the call
/// can become ambiguous now that one type satisfies both traits.
/// Resolve with UFCS — `<MyType as HostedDep>::from_descriptor(bytes)`.
impl<T: HostedDep> AsyncHostedDep for T {
    fn descriptor(&self) -> Vec<u8> {
        <T as HostedDep>::descriptor(self)
    }

    fn from_descriptor(bytes: &[u8]) -> impl std::future::Future<Output = Self> + Send {
        std::future::ready(<T as HostedDep>::from_descriptor(bytes))
    }
}

#[cfg(test)]
mod hosted_dep_blanket_bridge_tests {
    use super::{AsyncHostedDep, HostedDep};
    // Need `Future` in scope to call `.poll(...)` on the pinned future
    // returned by the blanket-bridged `from_descriptor`.
    use std::future::Future;

    /// Test fixture: a Hosted dep that only implements the sync `HostedDep`
    /// trait. The blanket bridge must also expose it through the async API.
    #[derive(Debug, PartialEq, Eq)]
    struct SyncOnlyDep {
        bytes: Vec<u8>,
    }

    impl HostedDep for SyncOnlyDep {
        fn descriptor(&self) -> Vec<u8> {
            self.bytes.clone()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
            }
        }
    }

    /// Compile-time pin that the blanket `impl<T: HostedDep> AsyncHostedDep
    /// for T` covers a sync-only `HostedDep`. If a future change ever
    /// drops or narrows the blanket, this test fires because the function
    /// signature won't compile: it requires `T: AsyncHostedDep` and we
    /// pass a `SyncOnlyDep` (which only implements `HostedDep`).
    fn requires_async_hosted_dep<T: AsyncHostedDep>(_t: &T) {}

    #[test]
    fn blanket_impl_exposes_sync_hosted_dep_via_async_api() {
        let dep = SyncOnlyDep {
            bytes: vec![1, 2, 3, 4],
        };

        // 1. Compile-time witness: the bound `T: AsyncHostedDep` resolves
        //    for `SyncOnlyDep` because of the blanket bridge.
        requires_async_hosted_dep(&dep);

        // 2. Owner-side: `descriptor()` reachable through both traits and
        //    returns the same bytes.
        assert_eq!(
            <SyncOnlyDep as HostedDep>::descriptor(&dep),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            <SyncOnlyDep as AsyncHostedDep>::descriptor(&dep),
            vec![1, 2, 3, 4]
        );

        // 3. Worker-side: `from_descriptor(...)` reachable through the
        //    async API; the returned future resolves synchronously to the
        //    same value the sync API would produce. Driven without an
        //    executor by polling the future once (since it is
        //    `std::future::Ready`, the first poll completes).
        let fut = <SyncOnlyDep as AsyncHostedDep>::from_descriptor(&[7, 8, 9]);
        let mut fut = Box::pin(fut);
        let waker = futures_test_helpers::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => {
                assert_eq!(
                    value,
                    SyncOnlyDep {
                        bytes: vec![7, 8, 9]
                    },
                    "blanket-bridged from_descriptor must yield the same value the sync impl produces"
                );
            }
            std::task::Poll::Pending => panic!(
                "blanket-bridged from_descriptor must be immediately ready (std::future::ready)"
            ),
        }
    }

    /// Minimal no-op waker so the test doesn't pull in tokio just to
    /// poll a `std::future::Ready`.
    mod futures_test_helpers {
        use std::task::{RawWaker, RawWakerVTable, Waker};

        unsafe fn clone(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VTABLE)
        }
        unsafe fn wake(_: *const ()) {}
        unsafe fn wake_by_ref(_: *const ()) {}
        unsafe fn drop(_: *const ()) {}

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        pub fn noop_waker() -> Waker {
            // SAFETY: vtable functions are no-ops and never touch the
            // null data pointer.
            unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
        }
    }
}

/// User-facing trait that opts a dependency value into the `HostedRpc` sharing
/// strategy. Like [`HostedDep`], the owner lives in the
/// **parent test runner process** for the entire suite; unlike `Hosted`,
/// workers do NOT see the owner type — they see a separate `Stub` type
/// that calls back into the parent over the existing IPC socket through a
/// small generated method-dispatch table.
///
/// The implementor provides:
/// - `type Stub`: the worker-side handle type tests actually parameterise on.
/// - [`dispatch`](HostedRpcDep::dispatch): owner-side method dispatcher.
///   Receives a stable `method_idx` plus serialized argument bytes and
///   returns serialized result bytes (or a textual error).
/// - [`build_stub`](HostedRpcDep::build_stub): worker-side constructor.
///   Wraps the supplied [`HostedRpcChannel`] into a `Self::Stub` that
///   serialises calls and forwards them to the parent's dispatcher.
///
/// Calls to a HostedRpc dep are **serialised** on the parent side (owner is held
/// behind a single `Mutex`). Even logical `&self` methods do not run
/// concurrently, matching how most singleton service handles already behave
/// internally.
pub trait HostedRpcDep: Send + Sync + 'static {
    /// The worker-side handle type that tests parameterise on. Typically
    /// a small struct that holds a [`HostedRpcChannel`] and implements
    /// a user-defined trait by routing each method through the channel.
    type Stub: Send + Sync + 'static;

    /// Owner-side: handle one method call. `method_idx` is a stable per-method
    /// index assigned by the implementor (usually generated by the
    /// `#[hosted_rpc]` macro; for a manual stub, the implementor picks the
    /// indices). `args` is the worker-supplied serialized payload. Return
    /// `Ok(bytes)` on success or `Err(message)` on failure — the message is
    /// surfaced to the calling worker as [`HostedRpcError::Dispatch`].
    fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String>;

    /// Worker-side: build a `Self::Stub` over the channel that connects
    /// back to the parent's owner. Called once per worker subprocess at
    /// startup, before any test body runs.
    ///
    /// **Contract — `build_stub` must be cheap and side-effect free.**
    /// The runtime constructs one stub per registered HostedRpc dep at
    /// worker startup, *before* the worker has even received its first
    /// [`crate::ipc::IpcCommand::RunTest`]. In particular this means:
    ///
    /// - **Do NOT call `channel.call(...)` from `build_stub`** — there is
    ///   no test in flight yet, so the parent's command loop may legally
    ///   send a `RunTest` while the stub is blocked waiting for a reply,
    ///   and the IPC framing will desync.
    /// - **Do NOT block, do I/O, or do expensive work** — the stub is
    ///   built unconditionally for every registered HostedRpc dep, even
    ///   if the test filter doesn't pull it into the suite.
    /// - Stash the `channel` and any small caches on `Self::Stub`; defer
    ///   all RPC to actual method calls inside test bodies.
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub;
}

/// Dyn-safe entry point used by the parent runtime to dispatch incoming
/// [`crate::ipc::IpcResponse::HostedRpcCall`] frames to a type-erased owner
/// value. Auto-implemented for every [`HostedRpcDep`].
pub trait HostedRpcDispatcher: Send + Sync {
    fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String>;
}

impl<T: HostedRpcDep> HostedRpcDispatcher for T {
    fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
        <T as HostedRpcDep>::dispatch(self, method_idx, args)
    }
}

/// Async counterpart of [`HostedRpcDep`]. Implement this when the owner-side
/// method dispatcher needs to `.await` (e.g. controlling subprocesses, holding
/// `tokio::sync` locks, calling other async APIs).
///
/// No opt-in flag is required: under the `tokio` test runtime the runtime
/// always routes HostedRpc dispatch through `AsyncHostedRpcDep`, regardless
/// of whether the user wrote `impl HostedRpcDep` (covered by the blanket
/// bridge below) or `impl AsyncHostedRpcDep` directly. The `#[hosted_rpc]`
/// macro mirrors this transparency: if any trait method is `async fn`, the
/// generated dispatcher is async, otherwise it stays sync.
///
/// `build_stub` stays synchronous and the worker-side stub still calls a
/// synchronous [`HostedRpcChannel::call`] in its method bodies, exactly as
/// in [`HostedRpcDep`]; the only difference is that owner-side `dispatch`
/// returns a `Future` so it can await.
pub trait AsyncHostedRpcDep: Send + Sync + 'static {
    /// The worker-side handle type that tests parameterise on. Same shape
    /// as [`HostedRpcDep::Stub`].
    type Stub: Send + Sync + 'static;

    /// Owner-side: handle one method call asynchronously. `method_idx` is the
    /// per-method index assigned by the implementor / `#[hosted_rpc]` macro;
    /// `args` is the worker-supplied serialized payload.
    fn dispatch<'a>(
        &'a mut self,
        method_idx: u32,
        args: &'a [u8],
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send + 'a;

    /// Worker-side: build a `Self::Stub` over the channel that connects
    /// back to the parent's owner. Identical contract to
    /// [`HostedRpcDep::build_stub`].
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub;
}

/// Blanket bridge: every [`HostedRpcDep`] is automatically also an
/// [`AsyncHostedRpcDep`]. The bridged `dispatch` returns
/// [`std::future::ready`] so the bridge itself adds only an
/// immediately-ready future and the tokio runtime can await all HostedRpc
/// dispatch uniformly.
///
/// This lets the test-r runtime drive **every** HostedRpc dispatch through
/// one async path under the `tokio` runtime, regardless of whether the
/// owner's own implementation is sync (`impl HostedRpcDep for ...`) or
/// async (`impl AsyncHostedRpcDep for ...`). No annotation flag is needed
/// on `#[hosted_rpc]` or `#[test_dep]`; the implementor picks sync vs
/// async purely at the trait-impl call site.
///
/// **Coherence note:** on stable Rust, with this blanket impl in place
/// a concrete owner type cannot manually implement both `HostedRpcDep` and
/// `AsyncHostedRpcDep` — rustc rejects that as conflicting implementations.
/// That compile-time error is the intended signal that one of the two manual
/// impls is redundant and should be removed.
///
/// **Source-compat note:** if downstream code imports both
/// `HostedRpcDep` and `AsyncHostedRpcDep` into the same scope and calls
/// trait methods by method syntax (e.g. `MyOwner::build_stub(channel)`
/// or `owner.dispatch(idx, args)`), the call can become ambiguous now
/// that one type satisfies both traits. Resolve with UFCS —
/// `<MyOwner as HostedRpcDep>::build_stub(channel)`.
impl<T: HostedRpcDep> AsyncHostedRpcDep for T {
    type Stub = <T as HostedRpcDep>::Stub;

    fn dispatch<'a>(
        &'a mut self,
        method_idx: u32,
        args: &'a [u8],
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send + 'a {
        std::future::ready(<T as HostedRpcDep>::dispatch(self, method_idx, args))
    }

    fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
        <T as HostedRpcDep>::build_stub(channel)
    }
}

/// Object-safe sibling of [`AsyncHostedRpcDep`] used by the parent's
/// async owner cell. Auto-implemented for every [`AsyncHostedRpcDep`].
pub trait AsyncHostedRpcDispatcher: Send + Sync {
    fn dispatch<'a>(
        &'a mut self,
        method_idx: u32,
        args: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>;
}

impl<T: AsyncHostedRpcDep> AsyncHostedRpcDispatcher for T {
    fn dispatch<'a>(
        &'a mut self,
        method_idx: u32,
        args: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>> {
        Box::pin(<T as AsyncHostedRpcDep>::dispatch(self, method_idx, args))
    }
}

#[cfg(test)]
mod hosted_rpc_blanket_bridge_tests {
    use super::{AsyncHostedRpcDep, HostedRpcChannel, HostedRpcDep};

    /// Sync-only HostedRpc owner fixture: implements [`HostedRpcDep`].
    /// The blanket bridge must also expose it as [`AsyncHostedRpcDep`].
    struct SyncOnlyOwner {
        next: u64,
    }

    /// Worker-side stub stand-in — the bridge tests do not exercise
    /// channel-side dispatch, so the stub just stashes the channel.
    pub struct SyncOnlyStub {
        _channel: HostedRpcChannel,
    }

    impl HostedRpcDep for SyncOnlyOwner {
        type Stub = SyncOnlyStub;

        fn dispatch(&mut self, method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
            if method_idx == 0 {
                self.next += 1;
                Ok(self.next.to_be_bytes().to_vec())
            } else {
                Err(format!("SyncOnlyOwner: unknown method_idx {method_idx}"))
            }
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            SyncOnlyStub { _channel: channel }
        }
    }

    /// Compile-time pin that the blanket `impl<T: HostedRpcDep>
    /// AsyncHostedRpcDep for T` covers a sync-only `HostedRpcDep`. If a
    /// future change ever drops or narrows the blanket, this function's
    /// signature stops compiling: it requires `T: AsyncHostedRpcDep` and
    /// we hand it a `SyncOnlyOwner` (which only implements `HostedRpcDep`).
    fn requires_async_hosted_rpc_dep<T: AsyncHostedRpcDep>(_t: &T) {}

    #[test]
    fn blanket_impl_exposes_sync_hosted_rpc_dep_via_async_api() {
        let owner = SyncOnlyOwner { next: 0 };
        // 1. Compile-time witness: the bound `T: AsyncHostedRpcDep`
        //    resolves for `SyncOnlyOwner` because of the blanket bridge.
        requires_async_hosted_rpc_dep(&owner);
    }

    /// Driving the bridge async dispatch end-to-end requires a tokio
    /// runtime, so the runtime-only assertion is cfg-gated. The sync
    /// build keeps the compile-time witness above; this test extends
    /// it by actually polling the bridged future and checking the
    /// dispatched bytes match what the sync dispatcher would produce.
    #[cfg(feature = "tokio")]
    #[test]
    fn bridged_async_dispatch_round_trips_sync_owner_bytes() {
        let mut owner = SyncOnlyOwner { next: 0 };
        let rt = ::tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let bytes = rt
            .block_on(<SyncOnlyOwner as AsyncHostedRpcDep>::dispatch(
                &mut owner,
                0,
                &[],
            ))
            .expect("bridged dispatch must succeed");
        assert_eq!(
            bytes,
            1u64.to_be_bytes().to_vec(),
            "bridged async dispatch must yield the same bytes the sync impl produces"
        );
    }
}

/// Type-erased, parent-owned cell that holds the owner value behind a
/// `Mutex` and exposes a `&self` dispatch entry point. Constructed by the
/// macro-generated registration code on the parent (the `DependencyConstructor`
/// for a `HostedRpc` dep returns one of these wrapped in `Arc<dyn Any>`)
/// and kept alive in `_hosted_owners` for the suite's lifetime.
///
/// Two internal variants:
/// - `Sync` holds a [`HostedRpcDep`] dispatcher and supports the legacy
///   synchronous dispatch path. The sync runtime uses this exclusively.
/// - `Async` holds an [`AsyncHostedRpcDep`] dispatcher behind a
///   [`tokio::sync::Mutex`] so awaits inside the dispatcher don't
///   block other tokio tasks waiting for the lock. The tokio runtime
///   constructs this variant for HostedRpc registrations.
pub struct HostedRpcOwnerCell {
    inner: HostedRpcOwnerCellInner,
}

enum HostedRpcOwnerCellInner {
    Sync(Mutex<Box<dyn HostedRpcDispatcher>>),
    #[cfg(feature = "tokio")]
    Async(AsyncOwnerCell),
}

#[cfg(feature = "tokio")]
struct AsyncOwnerCell {
    /// Mirrors the sync `Mutex` poisoning semantics: `tokio::sync::Mutex`
    /// itself does not poison, so we track poisoning out-of-band via this
    /// flag. Once a dispatched call panics, every subsequent dispatch
    /// short-circuits with the stable `"hosted rpc owner poisoned"` error.
    poisoned: std::sync::atomic::AtomicBool,
    inner: tokio::sync::Mutex<Box<dyn AsyncHostedRpcDispatcher>>,
}

impl HostedRpcOwnerCell {
    /// Wrap a synchronous owner value into a `HostedRpcOwnerCell`. The owner
    /// type must implement [`HostedRpcDep`]. This is the back-compat
    /// constructor used by the sync runtime and by any manual hand-written
    /// fixture; the runtime never blocks on async dispatch through cells
    /// built this way.
    pub fn from_owner<T: HostedRpcDep>(owner: T) -> Self {
        Self {
            inner: HostedRpcOwnerCellInner::Sync(Mutex::new(
                Box::new(owner) as Box<dyn HostedRpcDispatcher>
            )),
        }
    }

    /// Wrap an owner value that exposes an async dispatcher into a
    /// `HostedRpcOwnerCell`. Accepts any [`AsyncHostedRpcDep`] — including
    /// every [`HostedRpcDep`] via the blanket bridge — so the tokio runtime
    /// can route both sync and async owners through one async dispatch path.
    #[cfg(feature = "tokio")]
    pub fn from_async_owner<T: AsyncHostedRpcDep>(owner: T) -> Self {
        Self {
            inner: HostedRpcOwnerCellInner::Async(AsyncOwnerCell {
                poisoned: std::sync::atomic::AtomicBool::new(false),
                inner: tokio::sync::Mutex::new(Box::new(owner) as Box<dyn AsyncHostedRpcDispatcher>),
            }),
        }
    }

    /// Construct a synchronous `HostedRpcOwnerCell` that dispatches
    /// against `&T` borrowed from a shared `Arc<T>`, rather than
    /// consuming `T` outright. Used exclusively by the
    /// `#[test_dep(scope = Hosted, worker = both(Trait))]` lowering so
    /// the parent-side dep map can hand the same `Arc<T>` to
    /// downstream consumers that take `&T` while the RPC view keeps
    /// dispatching to the same owner instance.
    ///
    /// The supplied `dispatch` closure is typically a thin wrapper
    /// around the `#[hosted_rpc]`-generated
    /// `dispatch_<snake>_shared(&T, method_idx, args)` helper.
    ///
    /// Calls remain serialized by the cell's internal `Mutex`, matching
    /// the existing [`Self::from_owner`] semantics.
    pub fn from_shared_owner_sync<T, F>(owner: Arc<T>, dispatch: F) -> Self
    where
        T: Send + Sync + 'static,
        F: Fn(&T, u32, &[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    {
        struct SharedDispatcher<T, F>
        where
            T: Send + Sync + 'static,
            F: Fn(&T, u32, &[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
        {
            owner: Arc<T>,
            dispatch: F,
        }

        impl<T, F> HostedRpcDispatcher for SharedDispatcher<T, F>
        where
            T: Send + Sync + 'static,
            F: Fn(&T, u32, &[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
        {
            fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
                (self.dispatch)(&self.owner, method_idx, args)
            }
        }

        let dispatcher: Box<dyn HostedRpcDispatcher> =
            Box::new(SharedDispatcher { owner, dispatch });
        Self {
            inner: HostedRpcOwnerCellInner::Sync(Mutex::new(dispatcher)),
        }
    }

    /// Async counterpart of [`Self::from_shared_owner_sync`] for the
    /// tokio runtime: dispatch against `&T` via an async closure that
    /// returns a boxed future. Used by the `worker = both(Trait)`
    /// lowering when an async owner constructor is in play, or when
    /// the trait declared `async fn` methods.
    ///
    /// Calls remain serialized by the async cell's `tokio::sync::Mutex`,
    /// matching the existing [`Self::from_async_owner`] semantics.
    #[cfg(feature = "tokio")]
    pub fn from_shared_owner_async<T, F>(owner: Arc<T>, dispatch: F) -> Self
    where
        T: Send + Sync + 'static,
        F: for<'a> Fn(
                &'a T,
                u32,
                &'a [u8],
            )
                -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>
            + Send
            + Sync
            + 'static,
    {
        struct SharedAsyncDispatcher<T, F>
        where
            T: Send + Sync + 'static,
            F: for<'a> Fn(
                    &'a T,
                    u32,
                    &'a [u8],
                )
                    -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>
                + Send
                + Sync
                + 'static,
        {
            owner: Arc<T>,
            dispatch: F,
        }

        impl<T, F> AsyncHostedRpcDispatcher for SharedAsyncDispatcher<T, F>
        where
            T: Send + Sync + 'static,
            F: for<'a> Fn(
                    &'a T,
                    u32,
                    &'a [u8],
                )
                    -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>
                + Send
                + Sync
                + 'static,
        {
            fn dispatch<'a>(
                &'a mut self,
                method_idx: u32,
                args: &'a [u8],
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>> {
                (self.dispatch)(&self.owner, method_idx, args)
            }
        }

        let dispatcher: Box<dyn AsyncHostedRpcDispatcher> =
            Box::new(SharedAsyncDispatcher { owner, dispatch });
        Self {
            inner: HostedRpcOwnerCellInner::Async(AsyncOwnerCell {
                poisoned: std::sync::atomic::AtomicBool::new(false),
                inner: tokio::sync::Mutex::new(dispatcher),
            }),
        }
    }

    /// Dispatch one method call synchronously. Catches owner panics and
    /// turns them into `Err("hosted rpc owner panicked: …")` so the
    /// dispatcher loop never dies. The lock is acquired *inside* the
    /// `catch_unwind` closure on purpose: when the owner panics, the
    /// `MutexGuard` drops during the unwind, which poisons the mutex.
    /// Every subsequent `dispatch` call then short-circuits with the
    /// stable `"hosted rpc owner poisoned"` error and does NOT retry the
    /// (possibly half-mutated) owner.
    ///
    /// For cells constructed via [`Self::from_async_owner`], synchronous
    /// dispatch is unsupported and the call returns
    /// `Err("hosted rpc owner cell uses the async dispatch path; use dispatch_async or dispatch_blocking")`.
    /// Note that under the tokio feature a plain `HostedRpcDep` owner may
    /// also end up in the async cell variant via the blanket bridge, so
    /// this branch is not strictly limited to user-authored async owners.
    /// The sync runtime never builds async cells, so this branch only
    /// fires in misuse cases.
    pub fn dispatch(&self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
        match &self.inner {
            HostedRpcOwnerCellInner::Sync(mtx) => sync_dispatch_inner(mtx, method_idx, args),
            #[cfg(feature = "tokio")]
            HostedRpcOwnerCellInner::Async(_) => Err(
                "hosted rpc owner cell uses the async dispatch path; use dispatch_async or dispatch_blocking"
                    .to_string(),
            ),
        }
    }

    /// Async dispatch entry point used by the tokio runtime's parent-side
    /// HostedRpc loop and by the in-process transport's `block_on` bridge.
    /// Works for both `Sync` and `Async` cell variants:
    ///
    /// - `Sync` variant: invokes the synchronous dispatcher inline (no
    ///   `await` actually happens).
    /// - `Async` variant: awaits the user's async dispatcher with panic
    ///   capture so an `await`-side panic poisons the cell.
    #[cfg(feature = "tokio")]
    pub async fn dispatch_async(&self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
        match &self.inner {
            HostedRpcOwnerCellInner::Sync(mtx) => sync_dispatch_inner(mtx, method_idx, args),
            HostedRpcOwnerCellInner::Async(cell) => {
                async_dispatch_inner(cell, method_idx, args).await
            }
        }
    }

    /// Synchronous bridge to [`Self::dispatch_async`] for sync call sites
    /// (such as [`InProcessHostedRpcTransport::call`]) that need to feed
    /// an async owner cell. Drives the future with
    /// [`tokio::task::block_in_place`] + [`tokio::runtime::Handle::block_on`]
    /// when an async cell is present, and falls back to the regular sync
    /// dispatch otherwise.
    ///
    /// `Sync` cells short-circuit through the regular sync path with no
    /// runtime requirement. `Async` cells require a running multi-thread
    /// Tokio runtime, matching the IPC transport's existing requirement.
    #[cfg(feature = "tokio")]
    pub fn dispatch_blocking(&self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
        match &self.inner {
            HostedRpcOwnerCellInner::Sync(mtx) => sync_dispatch_inner(mtx, method_idx, args),
            HostedRpcOwnerCellInner::Async(cell) => {
                let handle = tokio::runtime::Handle::try_current().map_err(|_| {
                    "hosted rpc owner is async-only and no Tokio runtime is active at the dispatch site"
                        .to_string()
                })?;
                // `block_in_place` panics on a `current_thread` runtime
                // even though `Handle::try_current()` succeeded. Probe
                // the runtime flavor and return a clean error instead
                // of hitting the panic — the API contract is
                // `Result<_, String>`.
                if !matches!(
                    handle.runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::MultiThread
                ) {
                    return Err(
                        "hosted rpc owner is async-only and the current Tokio runtime is not multi-threaded"
                            .to_string(),
                    );
                }
                tokio::task::block_in_place(|| {
                    handle.block_on(async_dispatch_inner(cell, method_idx, args))
                })
            }
        }
    }
}

fn sync_dispatch_inner(
    mtx: &Mutex<Box<dyn HostedRpcDispatcher>>,
    method_idx: u32,
    args: &[u8],
) -> Result<Vec<u8>, String> {
    // The lock acquire lives inside the catch_unwind closure on
    // purpose. If we acquired the lock outside and the user dispatch
    // panicked, the panic would be caught before the MutexGuard had a
    // chance to drop during unwinding, leaving the mutex healthy — and
    // we want it poisoned so that subsequent calls see a deterministic
    // "owner is dead" error rather than re-entering a half-mutated
    // owner value.
    let dispatch_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut guard = match mtx.lock() {
            Ok(g) => g,
            Err(_) => return Err("hosted rpc owner poisoned".to_string()),
        };
        guard.dispatch(method_idx, args)
    }));
    panic_payload_to_err(dispatch_result)
}

#[cfg(feature = "tokio")]
async fn async_dispatch_inner(
    cell: &AsyncOwnerCell,
    method_idx: u32,
    args: &[u8],
) -> Result<Vec<u8>, String> {
    use futures::FutureExt;
    use std::sync::atomic::Ordering;

    // Fast-path check: avoids acquiring the async mutex when the owner
    // has already been poisoned by an earlier panic.
    if cell.poisoned.load(Ordering::SeqCst) {
        return Err("hosted rpc owner poisoned".to_string());
    }
    let mut guard = cell.inner.lock().await;
    // Re-check inside the lock: a second dispatch can park on
    // `lock().await` *before* the first dispatch panics. Without this
    // re-check the second waiter would acquire the lock and re-enter
    // the (possibly half-mutated) owner because the poison flag is
    // only stored after the panicking task drops its guard. This
    // mirrors the std::sync::Mutex poisoning semantics the sync cell
    // gets for free.
    if cell.poisoned.load(Ordering::SeqCst) {
        return Err("hosted rpc owner poisoned".to_string());
    }
    let fut = std::panic::AssertUnwindSafe(async {
        AsyncHostedRpcDispatcher::dispatch(&mut **guard, method_idx, args).await
    });
    let outcome = fut.catch_unwind().await;
    match outcome {
        Ok(r) => {
            drop(guard);
            r
        }
        Err(payload) => {
            // Set the poison flag *while still holding the guard* so any
            // waiter that subsequently acquires the mutex sees the flag
            // on its in-lock re-check above and short-circuits without
            // re-entering the owner.
            cell.poisoned.store(true, Ordering::SeqCst);
            drop(guard);
            let msg = panic_payload_to_string(&payload);
            Err(format!("hosted rpc owner panicked: {msg}"))
        }
    }
}

fn panic_payload_to_err(
    dispatch_result: Result<Result<Vec<u8>, String>, Box<dyn Any + Send>>,
) -> Result<Vec<u8>, String> {
    match dispatch_result {
        Ok(r) => r,
        Err(payload) => {
            let msg = panic_payload_to_string(&payload);
            Err(format!("hosted rpc owner panicked: {msg}"))
        }
    }
}

fn panic_payload_to_string(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Support type for `#[test_dep(scope = Hosted, worker = both(T))]`.
///
/// One macro-emitted `worker = both(T)` registration is lowered into
/// **two** `RegisteredDependency` entries that both point at the same
/// parent-side owner — one for the descriptor (Hosted) view, one for
/// the RPC stub (HostedRpc) view. To keep the owner unique under
/// either view, both registrations route through a single
/// `HostedBothShared` cell created by the macro-emitted weak cache:
///
/// - the **descriptor view** asks for the cached descriptor bytes
///   (`HostedDep::descriptor` / `AsyncHostedDep::descriptor` is only
///   called once, on the first construction);
/// - the **RPC view** asks for the inner [`HostedRpcOwnerCell`], so
///   the parent-side dispatcher sees the same owner the descriptor
///   was derived from;
/// - the **parent-side owner getter** (used by downstream dep
///   constructors that take `&Owner`) downcasts to [`HostedBothShared`]
///   and pulls out [`Self::owner_arc`], a type-erased `Arc<T>` of the
///   very same owner the cell holds.
///
/// This is intentionally *not* a public end-user type; only the
/// macro-support helpers in [`crate::__test_r_make_hosted_both_shared`]
/// and friends construct one.
pub struct HostedBothShared {
    descriptor_bytes: Vec<u8>,
    /// Type-erased `Arc<T>` of the owner value. The RPC cell holds
    /// the same `Arc<T>` (cloned) and dispatches against `&T` via the
    /// `#[hosted_rpc]`-generated `&self` dispatcher helper, so parent
    /// consumers and the RPC view observe one and the same owner
    /// instance.
    owner: Arc<dyn Any + Send + Sync>,
    rpc_cell: Arc<HostedRpcOwnerCell>,
}

impl HostedBothShared {
    /// Wrap a pre-computed descriptor + type-erased owner handle + RPC
    /// owner cell for the `both` dep variant. The macro acquire helper
    /// is the canonical construction site.
    pub fn new(
        descriptor_bytes: Vec<u8>,
        owner: Arc<dyn Any + Send + Sync>,
        rpc_cell: Arc<HostedRpcOwnerCell>,
    ) -> Self {
        Self {
            descriptor_bytes,
            owner,
            rpc_cell,
        }
    }

    /// Borrow the cached descriptor bytes (computed once, on first
    /// construction).
    pub fn descriptor_bytes(&self) -> &[u8] {
        &self.descriptor_bytes
    }

    /// Cheap clone of the inner RPC owner cell `Arc`. The
    /// HostedRpc-view registration's `RpcFactory::owner_into_cell`
    /// hands this back to the runtime.
    pub fn rpc_cell(&self) -> Arc<HostedRpcOwnerCell> {
        self.rpc_cell.clone()
    }

    /// Downcast the type-erased owner handle back to `Arc<T>`. Used by
    /// the macro-generated owner getter so parent-side consumers that
    /// take `&T` can reach the singleton owner the RPC cell is holding
    /// behind a shared dispatcher.
    pub fn owner_arc<T>(&self) -> Arc<T>
    where
        T: Send + Sync + 'static,
    {
        Arc::clone(&self.owner)
            .downcast::<T>()
            .expect("HostedBothShared owner type mismatch")
    }
}

/// Error returned by [`HostedRpcChannel::call`] when an RPC fails.
#[derive(Debug, Clone)]
pub enum HostedRpcError {
    /// The owner-side dispatcher returned an error string (unknown method,
    /// codec error, panic in the user method, …).
    Dispatch(String),
    /// The IPC transport itself failed (worker disconnected, framing error,
    /// runtime not in spawn-workers mode, …).
    Transport(String),
}

impl std::fmt::Display for HostedRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostedRpcError::Dispatch(s) => write!(f, "hosted rpc dispatch error: {s}"),
            HostedRpcError::Transport(s) => write!(f, "hosted rpc transport error: {s}"),
        }
    }
}

impl std::error::Error for HostedRpcError {}

/// Trait implemented by the per-runner transport that workers use to send
/// RPCs to the parent's owner. The runtime provides a concrete IPC
/// implementation for the spawn-workers case and a direct in-process
/// implementation for `--nocapture` / single-process mode.
pub trait HostedRpcTransport: Send + Sync {
    /// Send one call and block until the reply arrives. `dep_id` is the
    /// dep's fully-qualified id (`{crate}::{module}::{name}`) used by the
    /// parent to route the call to the right owner.
    fn call(&self, dep_id: &str, method_idx: u32, args: Vec<u8>)
        -> Result<Vec<u8>, HostedRpcError>;
}

/// Per-dep channel handed to [`HostedRpcDep::build_stub`] on the worker side.
///
/// The stub holds this channel and calls [`HostedRpcChannel::call`] from
/// each of its method bodies; the channel takes care of dep-id routing,
/// serialization framing, and waiting for the parent's reply.
pub struct HostedRpcChannel {
    dep_id: String,
    transport: Arc<dyn HostedRpcTransport>,
}

impl HostedRpcChannel {
    /// Construct a channel that targets the dep identified by
    /// `dep_id` (a fully-qualified id) and uses the supplied transport.
    pub fn new(dep_id: String, transport: Arc<dyn HostedRpcTransport>) -> Self {
        Self { dep_id, transport }
    }

    /// The fully-qualified dep id this channel routes to. Stubs almost never
    /// need this directly, but it's exposed for diagnostics and tests.
    pub fn dep_id(&self) -> &str {
        &self.dep_id
    }

    /// Send one method call and block until the parent replies. `args` are
    /// already-serialized bytes; the stub method body owns the choice of
    /// codec.
    ///
    /// **Temporal invariant — only call this while a test body is actually
    /// running.** The transport assumes one
    /// HostedRpc request/reply pair per worker subprocess is in flight
    /// at a time *and* that the worker's main IPC command loop is idle
    /// (it only reads `Provide*` / `RunTest` between tests). Specifically:
    ///
    /// - **Do NOT call from `HostedRpcDep::build_stub`** — see that
    ///   method's docs for why.
    /// - **Do NOT call from background threads or detached tasks that
    ///   outlive the test body** — once the test returns the worker
    ///   sends `TestFinished` and the parent's next message will be a
    ///   `Provide*` / `RunTest`, which the transport's read side would
    ///   then misinterpret as a reply.
    /// - **Do NOT call from `Drop` / destructor-style cleanup or any
    ///   teardown hook that may fire after the test body has returned** —
    ///   that is just another form of "outside the test body" and has the
    ///   same IPC-framing-desync risk as a detached background thread.
    /// - Stub calls from inside the test body — directly or transitively
    ///   from helpers the test body awaits/blocks on — are the supported
    ///   shape.
    pub fn call(&self, method_idx: u32, args: Vec<u8>) -> Result<Vec<u8>, HostedRpcError> {
        self.transport.call(&self.dep_id, method_idx, args)
    }
}

impl Clone for HostedRpcChannel {
    fn clone(&self) -> Self {
        Self {
            dep_id: self.dep_id.clone(),
            transport: self.transport.clone(),
        }
    }
}

/// In-process transport used in `--nocapture` / single-process mode: the
/// stub calls the owner-side [`HostedRpcOwnerCell`] directly without
/// touching any IPC stream.
pub struct InProcessHostedRpcTransport {
    cells: HashMap<String, Arc<HostedRpcOwnerCell>>,
}

impl InProcessHostedRpcTransport {
    pub fn new(cells: HashMap<String, Arc<HostedRpcOwnerCell>>) -> Self {
        Self { cells }
    }
}

impl HostedRpcTransport for InProcessHostedRpcTransport {
    fn call(
        &self,
        dep_id: &str,
        method_idx: u32,
        args: Vec<u8>,
    ) -> Result<Vec<u8>, HostedRpcError> {
        let cell = self.cells.get(dep_id).ok_or_else(|| {
            HostedRpcError::Transport(format!("in-process HostedRpc: unknown dep id '{dep_id}'"))
        })?;
        // Under the tokio feature, route through `dispatch_blocking` so async
        // owners (and bridged sync owners stored in `Async` cells) are driven
        // by the surrounding multi-thread tokio runtime. Without the tokio
        // feature only sync cells exist, so the plain sync `dispatch` is fine.
        #[cfg(feature = "tokio")]
        let result = cell.dispatch_blocking(method_idx, &args);
        #[cfg(not(feature = "tokio"))]
        let result = cell.dispatch(method_idx, &args);
        result.map_err(HostedRpcError::Dispatch)
    }
}

/// Factory pair stored on a `HostedRpc` [`RegisteredDependency`]. The macro
/// emits a `RpcFactory` per registered HostedRpc dep so the runtime can
/// (a) wrap the constructor's output into a parent dispatcher cell, and
/// (b) build a worker-side stub from a channel.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct RpcFactory {
    /// Downcast the constructor's `Arc<dyn Any>` to the concrete
    /// `HostedRpcOwnerCell` for this dep.
    pub owner_into_cell: Arc<
        dyn (Fn(Arc<dyn Any + Send + Sync>) -> Arc<HostedRpcOwnerCell>) + Send + Sync + 'static,
    >,
    /// Build a worker-side stub (typed as the dep's `Stub` associated type)
    /// from the supplied channel, boxed as `Arc<dyn Any>`.
    pub build_stub:
        Arc<dyn (Fn(HostedRpcChannel) -> Arc<dyn Any + Send + Sync>) + Send + Sync + 'static>,
}

/// Sharing strategy declared on a `#[test_dep]`. Controls how the dependency
/// interacts with output capturing and parallel test execution.
///
/// See `book/src/design/sharing-strategy.md` for the full description.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
pub enum DepScope {
    /// Today's behaviour: a single materialized instance shared by every test.
    /// Forces single-threaded execution when output capturing is on, because
    /// the `Arc<dyn Any>` cannot cross the parent/worker process boundary.
    #[default]
    Shared,
    /// Each worker child materializes its own instance independently. Tests
    /// inside one worker share the instance.
    PerWorker,
    /// Parent runs the constructor once and produces wire bytes; each worker
    /// reconstructs a local instance from those bytes via the registered
    /// worker reconstructor (`worker_fn`).
    Cloneable,
    /// Owner runs once in the **parent** test runner process and stays alive
    /// for the entire suite. The parent produces a descriptor (via
    /// [`HostedDep::descriptor`]) and ships those descriptor bytes to every
    /// worker. Each worker reconstructs a handle via
    /// [`HostedDep::from_descriptor`]. The owner is held in the parent
    /// process so singleton services (TCP listeners, Docker containers,
    /// gRPC server clients, env-based runtimes) are not duplicated per
    /// worker.
    Hosted,
    /// Like [`Self::Hosted`], but the owner stays in the parent AND workers
    /// talk back to it via the runtime's built-in RPC layer instead of reaching
    /// out via their own transport. The dep implementor provides a
    /// [`HostedRpcDep`] impl (sync owners) — or, under the `tokio` feature,
    /// an [`AsyncHostedRpcDep`] impl (async owners) — on the owner type
    /// with a stub type, a method dispatch function, and a stub builder.
    HostedRpc,
}

impl DepScope {
    /// Returns `true` for scopes that materialize in the parent process and
    /// therefore force single-threaded fallback when capturing is on.
    pub fn requires_single_thread_when_capturing(&self) -> bool {
        matches!(self, DepScope::Shared)
    }

    /// Returns `true` for scopes the parent should still materialize even
    /// when it is otherwise delegating dependency construction to workers
    /// (i.e. `skip_creating_dependencies` is set). `Cloneable` deps need the
    /// parent to compute the wire form; `Hosted` / `HostedRpc` deps need
    /// the parent to hold the owner alive for the whole suite.
    pub fn parent_must_materialize_under_spawn_workers(&self) -> bool {
        matches!(
            self,
            DepScope::Cloneable | DepScope::Hosted | DepScope::HostedRpc
        )
    }
}

/// Function pointer-equivalent used by the worker side of a `Cloneable`
/// dependency. Receives the deserialized wire payload (boxed as `Any` for
/// type erasure) plus the current dependency view, and produces the
/// reconstructed worker-side value.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub enum WorkerReconstructor {
    Sync(
        Arc<
            dyn (Fn(
                    Arc<dyn Any + Send + Sync>,
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Arc<dyn Any + Send + Sync + 'static>)
                + Send
                + Sync
                + 'static,
        >,
    ),
    Async(
        Arc<
            dyn (Fn(
                    Arc<dyn Any + Send + Sync>,
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = Arc<dyn Any + Send + Sync>>>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

/// Function-pointer wrappers used by Cloneable deps to convert the
/// constructed value into wire bytes on the parent, and to deserialize those
/// bytes into a typed value on the worker.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct CloneableCodec {
    /// Parent-side: `to_wire`. Receives the dependency value as `Arc<dyn Any>`,
    /// returns the encoded wire bytes.
    pub to_wire: Arc<dyn (Fn(Arc<dyn Any + Send + Sync>) -> Vec<u8>) + Send + Sync + 'static>,
    /// Worker-side: deserialize wire bytes into the boxed `Wire` payload that
    /// is then fed to the [`WorkerReconstructor`].
    pub from_wire_bytes: Arc<dyn (Fn(&[u8]) -> Arc<dyn Any + Send + Sync>) + Send + Sync + 'static>,
}

#[derive(Clone)]
pub struct RegisteredDependency {
    pub name: String, // TODO: Should we use TypeId here?
    pub crate_name: String,
    pub module_path: String,
    pub constructor: DependencyConstructor,
    pub dependencies: Vec<String>,
    /// Sharing strategy declared on the constructor. Defaults to
    /// [`DepScope::Shared`] for backward compatibility.
    pub scope: DepScope,
    /// Worker-side reconstructor for `Cloneable` and `Hosted` deps
    /// (`None` otherwise). For `Cloneable` the wire payload IS the dep value;
    /// for `Hosted` the wire payload is the descriptor passed to
    /// [`HostedDep::from_descriptor`](crate::internal::HostedDep::from_descriptor).
    pub worker_fn: Option<WorkerReconstructor>,
    /// Wire-bytes codec for `Cloneable` deps (`None` otherwise). The codec
    /// shape is shared with [`Self::hosted_codec`] but the runtime dispatches
    /// on whichever field is populated.
    pub cloneable_codec: Option<CloneableCodec>,
    /// Descriptor-bytes codec for `Hosted` deps (`None` otherwise). Same
    /// shape as [`Self::cloneable_codec`]; the codec encodes the value
    /// returned by [`HostedDep::descriptor`](crate::internal::HostedDep::descriptor)
    /// into wire bytes on the parent (where the owner lives), and decodes
    /// those bytes in the worker before they are passed to the registered
    /// worker reconstructor.
    pub hosted_codec: Option<CloneableCodec>,
    /// Factories for `HostedRpc` deps (`None` otherwise). The parent uses
    /// [`RpcFactory::owner_into_cell`] to extract the `HostedRpcOwnerCell`
    /// returned by the constructor; the worker uses [`RpcFactory::build_stub`]
    /// to construct its `Stub` from a fresh [`HostedRpcChannel`].
    pub rpc_factory: Option<RpcFactory>,
    /// Planner-only sibling dep names that must be retained together
    /// with this dep during pruning. Unlike `dependencies`, companions
    /// are **not** real dependency edges — no constructor argument is
    /// derived from a companion, and no topological ordering is
    /// implied. The pruner simply treats companions as mutually
    /// reachable: if any companion in a group is in the keep-set, the
    /// whole group is retained.
    ///
    /// Currently set by the `#[test_dep(scope = Hosted, worker = both(T))]`
    /// macro lowering, which registers two paired dep entries (the
    /// Hosted owner view and the HostedRpc stub view) backed by the
    /// same parent-side `Arc<HostedBothShared>` cache. The async
    /// flavour of that lowering has a sync resolver on the stub side
    /// that assumes the Hosted side has already populated the shared
    /// cache; if pruning ever dropped the Hosted half because the
    /// selected tests only parameterised on the stub view, that
    /// resolver would panic. Pairing the two as companions guarantees
    /// the Hosted half is retained whenever either half is needed.
    pub companions: Vec<String>,
}

impl RegisteredDependency {
    /// Construct a `Shared` (legacy / default-scope) dependency. Preserves the
    /// pre-scopes constructor signature so downstream code that built
    /// `RegisteredDependency` directly keeps compiling.
    pub fn new_shared(
        name: String,
        crate_name: String,
        module_path: String,
        constructor: DependencyConstructor,
        dependencies: Vec<String>,
    ) -> Self {
        Self {
            name,
            crate_name,
            module_path,
            constructor,
            dependencies,
            scope: DepScope::Shared,
            worker_fn: None,
            cloneable_codec: None,
            hosted_codec: None,
            rpc_factory: None,
            companions: Vec::new(),
        }
    }
}

impl Debug for RegisteredDependency {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredDependency")
            .field("name", &self.name)
            .field("crate_name", &self.crate_name)
            .field("module_path", &self.module_path)
            .finish()
    }
}

impl PartialEq for RegisteredDependency {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for RegisteredDependency {}

impl Hash for RegisteredDependency {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl RegisteredDependency {
    pub fn crate_and_module(&self) -> String {
        [&self.crate_name, &self.module_path]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }

    /// Fully-qualified identifier used for cross-process bookkeeping of
    /// Cloneable dependencies. The shape is `{crate_name}::{module_path}::{name}`
    /// with empty segments dropped, so two deps with the same `name` registered
    /// in different modules get distinct identifiers.
    pub fn qualified_id(&self) -> String {
        [&self.crate_name, &self.module_path, &self.name]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

pub static REGISTERED_DEPENDENCY_CONSTRUCTORS: Mutex<Vec<RegisteredDependency>> =
    Mutex::new(Vec::new());

#[derive(Debug, Clone)]
pub enum RegisteredTestSuiteProperty {
    Sequential {
        name: String,
        crate_name: String,
        module_path: String,
    },
    Tag {
        name: String,
        crate_name: String,
        module_path: String,
        tag: String,
    },
    Timeout {
        name: String,
        crate_name: String,
        module_path: String,
        timeout: Duration,
    },
}

impl RegisteredTestSuiteProperty {
    pub fn crate_name(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { crate_name, .. } => crate_name,
            RegisteredTestSuiteProperty::Tag { crate_name, .. } => crate_name,
            RegisteredTestSuiteProperty::Timeout { crate_name, .. } => crate_name,
        }
    }

    pub fn module_path(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { module_path, .. } => module_path,
            RegisteredTestSuiteProperty::Tag { module_path, .. } => module_path,
            RegisteredTestSuiteProperty::Timeout { module_path, .. } => module_path,
        }
    }

    pub fn name(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { name, .. } => name,
            RegisteredTestSuiteProperty::Tag { name, .. } => name,
            RegisteredTestSuiteProperty::Timeout { name, .. } => name,
        }
    }

    pub fn crate_and_module(&self) -> String {
        [self.crate_name(), self.module_path(), self.name()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

pub static REGISTERED_TESTSUITE_PROPS: Mutex<Vec<RegisteredTestSuiteProperty>> =
    Mutex::new(Vec::new());

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub enum TestGeneratorFunction {
    Sync(Arc<dyn Fn() -> Vec<GeneratedTest> + Send + Sync + 'static>),
    Async(
        Arc<
            dyn (Fn() -> Pin<Box<dyn Future<Output = Vec<GeneratedTest>> + Send>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

pub struct DynamicTestRegistration {
    tests: Vec<GeneratedTest>,
}

impl Default for DynamicTestRegistration {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicTestRegistration {
    pub fn new() -> Self {
        Self { tests: Vec::new() }
    }

    pub fn to_vec(self) -> Vec<GeneratedTest> {
        self.tests
    }

    pub fn add_sync_test<R: TestReturnValue + 'static>(
        &mut self,
        name: impl AsRef<str>,
        props: TestProperties,
        dependencies: Option<Vec<String>>,
        run: impl Fn(Arc<dyn DependencyView + Send + Sync>) -> R + Send + Sync + Clone + 'static,
    ) {
        self.tests.push(GeneratedTest {
            name: name.as_ref().to_string(),
            run: TestFunction::Sync(Arc::new(move |deps| {
                Box::new(run(deps)) as Box<dyn TestReturnValue>
            })),
            props,
            dependencies,
        });
    }

    #[cfg(feature = "tokio")]
    pub fn add_async_test<R: TestReturnValue + 'static>(
        &mut self,
        name: impl AsRef<str>,
        props: TestProperties,
        dependencies: Option<Vec<String>>,
        run: impl (Fn(Arc<dyn DependencyView + Send + Sync>) -> Pin<Box<dyn Future<Output = R> + Send>>)
            + Send
            + Sync
            + Clone
            + 'static,
    ) {
        self.tests.push(GeneratedTest {
            name: name.as_ref().to_string(),
            run: TestFunction::Async(Arc::new(move |deps| {
                let run = run.clone();
                Box::pin(async move {
                    let r = run(deps).await;
                    Box::new(r) as Box<dyn TestReturnValue>
                })
            })),
            props,
            dependencies,
        });
    }
}

#[derive(Clone)]
pub struct GeneratedTest {
    pub name: String,
    pub run: TestFunction,
    pub props: TestProperties,
    pub dependencies: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct RegisteredTestGenerator {
    pub name: String,
    pub crate_name: String,
    pub module_path: String,
    pub run: TestGeneratorFunction,
    pub is_ignored: bool,
}

impl RegisteredTestGenerator {
    pub fn crate_and_module(&self) -> String {
        [&self.crate_name, &self.module_path]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

pub static REGISTERED_TEST_GENERATORS: Mutex<Vec<RegisteredTestGenerator>> = Mutex::new(Vec::new());

pub(crate) fn filter_test(test: &RegisteredTest, filter: &str, exact: bool) -> bool {
    if let Some(tag_list) = filter.strip_prefix(":tag:") {
        if tag_list.is_empty() {
            // Filtering for tags with NO TAGS
            test.props.tags.is_empty()
        } else {
            let or_tags = tag_list.split('|').collect::<Vec<&str>>();
            let mut result = false;
            for or_tag in or_tags {
                let and_tags = or_tag.split('&').collect::<Vec<&str>>();
                let mut and_result = true;
                for and_tag in and_tags {
                    if !test.props.tags.contains(&and_tag.to_string()) {
                        and_result = false;
                        break;
                    }
                }
                if and_result {
                    result = true;
                    break;
                }
            }
            result
        }
    } else if exact {
        test.filterable_name() == filter
    } else {
        test.filterable_name().contains(filter)
    }
}

pub(crate) fn apply_suite_props_to_tests(
    tests: &[RegisteredTest],
    props: &[RegisteredTestSuiteProperty],
) -> Vec<RegisteredTest> {
    let props_with_prefix = props
        .iter()
        .map(|prop| (prop.crate_and_module(), prop))
        .collect::<Vec<_>>();

    let mut result = Vec::new();
    for test in tests {
        let mut test = test.clone();
        for (prefix, prop) in &props_with_prefix {
            if test.crate_and_module().starts_with(prefix) {
                match prop {
                    RegisteredTestSuiteProperty::Tag { tag, .. } => {
                        test.props.tags.push(tag.clone());
                    }
                    RegisteredTestSuiteProperty::Timeout { timeout, .. } => {
                        if test.props.timeout.is_none() {
                            test.props.timeout = Some(*timeout);
                        }
                    }
                    RegisteredTestSuiteProperty::Sequential { .. } => {
                        // handled in TestSuiteExecution
                    }
                }
            }
        }
        result.push(test);
    }
    result
}

pub(crate) fn filter_registered_tests(
    args: &Arguments,
    registered_tests: &[RegisteredTest],
) -> Vec<RegisteredTest> {
    registered_tests
        .iter()
        .filter(|registered_test| {
            !args
                .skip
                .iter()
                .any(|skip| filter_test(registered_test, skip, args.exact))
        })
        .filter(|registered_test| {
            args.filter.is_empty()
                || args
                    .filter
                    .iter()
                    .any(|filter| filter_test(registered_test, filter, args.exact))
        })
        .filter(|registered_tests| {
            (args.bench && registered_tests.run.is_bench())
                || (args.test && !registered_tests.run.is_bench())
                || (!args.bench && !args.test)
        })
        .filter(|registered_test| {
            !args.exclude_should_panic || registered_test.props.should_panic == ShouldPanic::No
        })
        .cloned()
        .collect::<Vec<_>>()
}

fn add_generated_tests(
    target: &mut Vec<RegisteredTest>,
    generator: &RegisteredTestGenerator,
    generated: Vec<GeneratedTest>,
) {
    target.extend(generated.into_iter().map(|mut test| {
        test.props.is_ignored |= generator.is_ignored;
        RegisteredTest {
            name: format!("{}::{}", generator.name, test.name),
            crate_name: generator.crate_name.clone(),
            module_path: generator.module_path.clone(),
            run: test.run,
            props: test.props,
            dependencies: test.dependencies,
        }
    }));
}

#[cfg(feature = "tokio")]
pub(crate) async fn generate_tests(generators: &[RegisteredTestGenerator]) -> Vec<RegisteredTest> {
    let mut result = Vec::new();
    for generator in generators {
        match &generator.run {
            TestGeneratorFunction::Sync(generator_fn) => {
                let tests = generator_fn();
                add_generated_tests(&mut result, generator, tests);
            }
            TestGeneratorFunction::Async(generator_fn) => {
                let tests = generator_fn().await;
                add_generated_tests(&mut result, generator, tests);
            }
        }
    }
    result
}

pub(crate) fn generate_tests_sync(generators: &[RegisteredTestGenerator]) -> Vec<RegisteredTest> {
    let mut result = Vec::new();
    for generator in generators {
        match &generator.run {
            TestGeneratorFunction::Sync(generator_fn) => {
                let tests = generator_fn();
                add_generated_tests(&mut result, generator, tests);
            }
            TestGeneratorFunction::Async(_) => {
                panic!("Async test generators are not supported in sync mode")
            }
        }
    }
    result
}

pub(crate) fn get_ensure_time(args: &Arguments, test: &RegisteredTest) -> Option<TimeThreshold> {
    let should_ensure_time = match test.props.ensure_time_control {
        ReportTimeControl::Default => args.ensure_time,
        ReportTimeControl::Enabled => true,
        ReportTimeControl::Disabled => false,
    };
    if should_ensure_time {
        match test.props.test_type {
            TestType::UnitTest => Some(args.unit_test_threshold()),
            TestType::IntegrationTest => Some(args.integration_test_threshold()),
        }
    } else {
        None
    }
}

#[derive(Clone)]
pub enum TestResult {
    Passed {
        captured: Vec<CapturedOutput>,
        exec_time: Duration,
    },
    Benchmarked {
        captured: Vec<CapturedOutput>,
        exec_time: Duration,
        ns_iter_summ: Summary,
        mb_s: usize,
    },
    Failed {
        cause: FailureCause,
        captured: Vec<CapturedOutput>,
        exec_time: Duration,
    },
    Ignored {
        captured: Vec<CapturedOutput>,
    },
}

impl TestResult {
    pub fn passed(exec_time: Duration) -> Self {
        TestResult::Passed {
            captured: Vec::new(),
            exec_time,
        }
    }

    pub fn benchmarked(exec_time: Duration, ns_iter_summ: Summary, mb_s: usize) -> Self {
        TestResult::Benchmarked {
            captured: Vec::new(),
            exec_time,
            ns_iter_summ,
            mb_s,
        }
    }

    pub fn failed(exec_time: Duration, cause: FailureCause) -> Self {
        TestResult::Failed {
            cause,
            captured: Vec::new(),
            exec_time,
        }
    }

    pub fn ignored() -> Self {
        TestResult::Ignored {
            captured: Vec::new(),
        }
    }

    pub(crate) fn is_passed(&self) -> bool {
        matches!(self, TestResult::Passed { .. })
    }

    pub(crate) fn is_benchmarked(&self) -> bool {
        matches!(self, TestResult::Benchmarked { .. })
    }

    pub(crate) fn is_failed(&self) -> bool {
        matches!(self, TestResult::Failed { .. })
    }

    pub(crate) fn is_ignored(&self) -> bool {
        matches!(self, TestResult::Ignored { .. })
    }

    pub(crate) fn captured_output(&self) -> &Vec<CapturedOutput> {
        match self {
            TestResult::Passed { captured, .. } => captured,
            TestResult::Failed { captured, .. } => captured,
            TestResult::Ignored { captured, .. } => captured,
            TestResult::Benchmarked { captured, .. } => captured,
        }
    }

    pub(crate) fn stats(&self) -> Option<&Summary> {
        match self {
            TestResult::Benchmarked { ns_iter_summ, .. } => Some(ns_iter_summ),
            _ => None,
        }
    }

    pub(crate) fn set_captured_output(&mut self, captured: Vec<CapturedOutput>) {
        match self {
            TestResult::Passed {
                captured: captured_ref,
                ..
            } => *captured_ref = captured,
            TestResult::Failed {
                captured: captured_ref,
                ..
            } => *captured_ref = captured,
            TestResult::Ignored {
                captured: captured_ref,
            } => *captured_ref = captured,
            TestResult::Benchmarked {
                captured: captured_ref,
                ..
            } => *captured_ref = captured,
        }
    }

    pub(crate) fn from_result<A>(
        should_panic: &ShouldPanic,
        elapsed: Duration,
        result: Result<Result<A, FailureCause>, Box<dyn Any + Send>>,
    ) -> Self {
        match result {
            Ok(Ok(_)) => {
                if should_panic == &ShouldPanic::No {
                    TestResult::passed(elapsed)
                } else {
                    TestResult::failed(
                        elapsed,
                        FailureCause::HarnessError("Test did not panic as expected".to_string()),
                    )
                }
            }
            Ok(Err(cause)) => TestResult::failed(elapsed, cause),
            Err(panic) => TestResult::from_panic(should_panic, elapsed, panic),
        }
    }

    pub(crate) fn from_summary(
        should_panic: &ShouldPanic,
        elapsed: Duration,
        result: Result<Summary, Box<dyn Any + Send>>,
        bytes: u64,
    ) -> Self {
        match result {
            Ok(summary) => {
                let ns_iter = max(summary.median as u64, 1);
                let mb_s = bytes * 1000 / ns_iter;
                TestResult::benchmarked(elapsed, summary, mb_s as usize)
            }
            Err(panic) => Self::from_panic(should_panic, elapsed, panic),
        }
    }

    fn from_panic(
        should_panic: &ShouldPanic,
        elapsed: Duration,
        panic: Box<dyn Any + Send>,
    ) -> Self {
        let captured = crate::panic_hook::take_current_panic_capture();

        let panic_cause = if let Some(cause) = captured {
            cause
        } else {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or(panic.downcast_ref::<&str>().map(|s| s.to_string()));
            PanicCause {
                message,
                location: None,
                backtrace: None,
            }
        };

        match should_panic {
            ShouldPanic::WithMessage(expected) => match &panic_cause.message {
                Some(message) if message.contains(expected) => TestResult::passed(elapsed),
                _ => TestResult::failed(
                    elapsed,
                    FailureCause::Panic(PanicCause {
                        message: Some(format!(
                            "Test panicked with unexpected message: {}",
                            panic_cause.message.as_deref().unwrap_or_default()
                        )),
                        location: None,
                        backtrace: None,
                    }),
                ),
            },
            ShouldPanic::Yes => TestResult::passed(elapsed),
            ShouldPanic::No => TestResult::failed(elapsed, FailureCause::Panic(panic_cause)),
        }
    }

    pub(crate) fn failure_message(&self) -> Option<String> {
        self.failure_cause().map(|c| c.render())
    }

    pub fn failure_cause(&self) -> Option<&FailureCause> {
        match self {
            TestResult::Failed { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

pub struct SuiteResult {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub measured: usize,
    pub filtered_out: usize,
    pub exec_time: Duration,
}

impl SuiteResult {
    pub fn from_test_results(
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) -> Self {
        let passed = results
            .iter()
            .filter(|(_, result)| result.is_passed())
            .count();
        let measured = results
            .iter()
            .filter(|(_, result)| result.is_benchmarked())
            .count();
        let failed = results
            .iter()
            .filter(|(_, result)| result.is_failed())
            .count();
        let ignored = results
            .iter()
            .filter(|(_, result)| result.is_ignored())
            .count();
        let filtered_out = registered_tests.len() - results.len();

        Self {
            passed,
            failed,
            ignored,
            measured,
            filtered_out,
            exec_time,
        }
    }

    pub fn exit_code(results: &[(RegisteredTest, TestResult)]) -> ExitCode {
        if results.iter().any(|(_, result)| result.is_failed()) {
            ExitCode::from(101)
        } else {
            ExitCode::SUCCESS
        }
    }
}

pub trait DependencyView: Debug {
    fn get(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>>;
}

impl DependencyView for Arc<dyn DependencyView + Send + Sync> {
    fn get(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.as_ref().get(name)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CapturedOutput {
    Stdout {
        timestamp: SystemTime,
        line: String,
    },
    Stderr {
        timestamp: SystemTime,
        line: String,
    },
    /// Host-side output captured in the parent process during this
    /// test's execution window. Attribution is overlap-based and
    /// best-effort: a line that lands on the parent's redirected
    /// stdout/stderr pipe between the parent observing this test
    /// start and finish is attributed to it. Sources include
    /// `HostedRpc` owner dispatch methods, owner constructors that
    /// are still emitting after returning, and any background
    /// threads / tasks / subprocesses they spawn.
    ///
    /// When tests run in parallel a single host-side line may be
    /// attributed to multiple tests whose windows overlap.
    Host {
        timestamp: SystemTime,
        line: String,
    },
}

impl CapturedOutput {
    pub fn stdout(line: String) -> Self {
        CapturedOutput::Stdout {
            timestamp: SystemTime::now(),
            line,
        }
    }

    pub fn stderr(line: String) -> Self {
        CapturedOutput::Stderr {
            timestamp: SystemTime::now(),
            line,
        }
    }

    /// Constructs a `Host`-tagged capture. Used by the parent's host
    /// capture finaliser to inject overlap-attributed host log lines
    /// into each test's captured output vec before the formatter
    /// renders the suite.
    pub fn host(timestamp: SystemTime, line: String) -> Self {
        CapturedOutput::Host { timestamp, line }
    }

    pub fn timestamp(&self) -> SystemTime {
        match self {
            CapturedOutput::Stdout { timestamp, .. } => *timestamp,
            CapturedOutput::Stderr { timestamp, .. } => *timestamp,
            CapturedOutput::Host { timestamp, .. } => *timestamp,
        }
    }

    pub fn line(&self) -> &str {
        match self {
            CapturedOutput::Stdout { line, .. } => line,
            CapturedOutput::Stderr { line, .. } => line,
            CapturedOutput::Host { line, .. } => line,
        }
    }
}

impl PartialOrd for CapturedOutput {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CapturedOutput {
    fn cmp(&self, other: &Self) -> Ordering {
        self.timestamp().cmp(&other.timestamp())
    }
}

#[cfg(test)]
mod error_reporting_tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::time::Duration;

    fn simulate_runner(
        test_fn: impl FnOnce() -> Box<dyn TestReturnValue> + std::panic::UnwindSafe,
    ) -> TestResult {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        let result = catch_unwind(AssertUnwindSafe(move || {
            let ret = test_fn();
            ret.into_result()?;
            Ok(())
        }));
        let test_result =
            TestResult::from_result(&ShouldPanic::No, Duration::from_millis(1), result);
        crate::panic_hook::clear_current_test_id();
        test_result
    }

    #[test]
    fn panic_with_assert_eq() {
        let result = simulate_runner(|| {
            assert_eq!(1, 2);
            Box::new(())
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== panic assert_eq failure message ===\n{msg}\n===");
        assert!(
            msg.contains("assertion `left == right` failed"),
            "Expected assertion message, got: {msg}"
        );
        assert!(
            msg.contains("at "),
            "Expected location info in message, got: {msg}"
        );
    }

    #[test]
    fn string_error() {
        let result = simulate_runner(|| {
            let r: Result<(), String> = Err("something went wrong".to_string());
            Box::new(r)
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== string error failure message ===\n{msg}\n===");
        assert_eq!(msg, "something went wrong");
    }

    #[test]
    fn anyhow_error() {
        let result = simulate_runner(|| {
            let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
            let err = anyhow::anyhow!(inner).context("operation failed");
            let r: Result<(), anyhow::Error> = Err(err);
            Box::new(r)
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== anyhow error failure message ===\n{msg}\n===");
        assert!(
            msg.contains("operation failed"),
            "Expected 'operation failed', got: {msg}"
        );
        assert!(
            msg.contains("file not found"),
            "Expected 'file not found', got: {msg}"
        );
    }

    #[test]
    fn std_io_error() {
        let result = simulate_runner(|| {
            let r: Result<(), std::io::Error> = Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "file not found",
            ));
            Box::new(r)
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== std io error failure message ===\n{msg}\n===");
        // Should use Display (not Debug), so no "Custom { kind: NotFound, ... }"
        assert_eq!(msg, "file not found");
    }

    #[test]
    fn panic_with_location_info() {
        let result = simulate_runner(|| {
            panic!("test panic with location");
            #[allow(unreachable_code)]
            Box::new(())
        });
        assert!(result.is_failed());
        let cause = result.failure_cause().unwrap();
        match cause {
            FailureCause::Panic(p) => {
                assert!(p.location.is_some(), "Expected location info");
                let loc = p.location.as_ref().unwrap();
                assert!(
                    loc.file.contains("internal.rs"),
                    "Expected file to contain internal.rs, got: {}",
                    loc.file
                );
                assert!(loc.line > 0, "Expected non-zero line number");
            }
            other => panic!("Expected Panic cause, got: {other:?}"),
        }
    }

    #[test]
    fn panic_render_includes_location() {
        let result = simulate_runner(|| {
            panic!("location test");
            #[allow(unreachable_code)]
            Box::new(())
        });
        let msg = result.failure_message().unwrap();
        assert!(
            msg.contains("location test"),
            "Expected panic message, got: {msg}"
        );
        assert!(
            msg.contains("\n  at "),
            "Expected location line in render, got: {msg}"
        );
    }

    #[test]
    fn should_panic_with_message_matching() {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        let result = catch_unwind(AssertUnwindSafe(|| {
            panic!("expected panic message");
        }));
        let test_result = TestResult::from_result(
            &ShouldPanic::WithMessage("expected panic".to_string()),
            Duration::from_millis(1),
            result.map(|_| Ok(())),
        );
        crate::panic_hook::clear_current_test_id();
        assert!(
            test_result.is_passed(),
            "Expected test to pass with matching panic message"
        );
    }

    #[test]
    fn should_panic_with_wrong_message() {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        let result = catch_unwind(AssertUnwindSafe(|| {
            panic!("actual panic message");
        }));
        let test_result = TestResult::from_result(
            &ShouldPanic::WithMessage("completely different".to_string()),
            Duration::from_millis(1),
            result.map(|_| Ok(())),
        );
        crate::panic_hook::clear_current_test_id();
        assert!(
            test_result.is_failed(),
            "Expected test to fail with wrong panic message"
        );
        let msg = test_result.failure_message().unwrap();
        assert!(
            msg.contains("unexpected message"),
            "Expected 'unexpected message' in: {msg}"
        );
    }

    #[test]
    fn pretty_assertions_diff() {
        let result = simulate_runner(|| {
            pretty_assertions::assert_eq!("hello world\nfoo\nbar\n", "hello world\nbaz\nbar\n");
            Box::new(())
        });
        assert!(result.is_failed());
        let cause = result.failure_cause().unwrap();

        // Should be a Panic variant (assert_eq! panics)
        let panic_cause = match cause {
            FailureCause::Panic(p) => p,
            other => panic!("Expected Panic cause, got: {other:?}"),
        };

        // The panic message should contain the colorful diff from pretty_assertions
        let message = panic_cause.message.as_deref().unwrap();
        println!("=== pretty_assertions failure message ===\n{message}\n===");
        assert!(
            message.contains("foo") && message.contains("baz"),
            "Expected diff with 'foo' and 'baz', got: {message}"
        );

        // Location should be captured
        assert!(panic_cause.location.is_some(), "Expected location info");

        // The rendered output should NOT contain backtrace noise when RUST_BACKTRACE is unset
        let rendered = cause.render();
        println!("=== pretty_assertions rendered ===\n{rendered}\n===");
        assert!(
            !rendered.contains("stack backtrace") && !rendered.contains("Stack backtrace"),
            "Expected no backtrace noise in rendered output, got: {rendered}"
        );
        // Should contain location
        assert!(
            rendered.contains("\n  at "),
            "Expected location in rendered output, got: {rendered}"
        );
    }

    #[test]
    fn detached_thread_panic_detected() {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        crate::panic_hook::create_detached_collector(test_id);

        let result = catch_unwind(AssertUnwindSafe(|| {
            let handle = crate::spawn::spawn_thread(|| {
                panic!("background thread panic");
            });
            let _ = handle.join();
        }));

        let mut test_result = TestResult::from_result(
            &ShouldPanic::No,
            Duration::from_millis(1),
            result.map(|_| Ok(())),
        );

        if let Some(collector) = crate::panic_hook::take_detached_collector(test_id) {
            let panics = match collector.lock() {
                Ok(p) => p,
                Err(poisoned) => poisoned.into_inner(),
            };
            if !panics.is_empty() && test_result.is_passed() {
                let messages: Vec<String> = panics.iter().map(|p| p.render()).collect();
                test_result = TestResult::failed(
                    Duration::from_millis(1),
                    FailureCause::Panic(PanicCause {
                        message: Some(format!(
                            "Detached task(s) panicked:\n{}",
                            messages.join("\n---\n")
                        )),
                        location: panics.first().and_then(|p| p.location.clone()),
                        backtrace: panics.first().and_then(|p| p.backtrace.clone()),
                    }),
                );
            }
        }

        crate::panic_hook::clear_current_test_id();

        assert!(
            test_result.is_failed(),
            "Expected test to fail due to detached panic"
        );
        let msg = test_result.failure_message().unwrap();
        assert!(
            msg.contains("Detached task(s) panicked"),
            "Expected detached panic message, got: {msg}"
        );
        assert!(
            msg.contains("background thread panic"),
            "Expected original panic message, got: {msg}"
        );
    }

    #[test]
    fn detached_thread_panic_ignored_with_policy() {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        crate::panic_hook::create_detached_collector(test_id);

        let result = catch_unwind(AssertUnwindSafe(|| {
            let handle = crate::spawn::spawn_thread(|| {
                panic!("ignored thread panic");
            });
            let _ = handle.join();
        }));

        let test_result = TestResult::from_result(
            &ShouldPanic::No,
            Duration::from_millis(1),
            result.map(|_| Ok(())),
        );

        if let Some(collector) = crate::panic_hook::take_detached_collector(test_id) {
            let panics = match collector.lock() {
                Ok(p) => p,
                Err(poisoned) => poisoned.into_inner(),
            };
            // Verify panics were captured but Ignore policy does not fail the test
            assert!(
                !panics.is_empty(),
                "Expected panics in collector even with Ignore policy"
            );
        }

        crate::panic_hook::clear_current_test_id();

        assert!(
            test_result.is_passed(),
            "Expected test to pass with Ignore policy"
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn detached_task_panic_detected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            crate::panic_hook::install_panic_hook();
            let test_id = crate::panic_hook::next_test_id();
            crate::panic_hook::set_current_test_id(test_id);
            crate::panic_hook::create_detached_collector(test_id);

            let handle = crate::spawn::spawn(async {
                panic!("detached task panic");
            });
            let _ = handle.await;

            let collector = crate::panic_hook::take_detached_collector(test_id).unwrap();
            let panics = collector.lock().unwrap();

            assert_eq!(panics.len(), 1);
            assert!(
                panics[0]
                    .message
                    .as_ref()
                    .unwrap()
                    .contains("detached task panic"),
                "Expected panic message, got: {:?}",
                panics[0].message
            );

            crate::panic_hook::clear_current_test_id();
        });
    }

    #[test]
    fn failure_cause_variants() {
        // ReturnedMessage
        let cause = FailureCause::ReturnedMessage("simple message".to_string());
        assert_eq!(cause.render(), "simple message");
        assert!(cause.panic_message().is_none());

        // ReturnedError (prefer display)
        let cause = FailureCause::ReturnedError {
            display: "display text".to_string(),
            debug: "debug text".to_string(),
            prefer_debug: false,
            error: Arc::new("display text".to_string()),
        };
        assert_eq!(cause.render(), "display text");

        // ReturnedError (prefer debug, e.g. anyhow)
        let cause = FailureCause::ReturnedError {
            display: "display text".to_string(),
            debug: "debug text".to_string(),
            prefer_debug: true,
            error: Arc::new("debug text".to_string()),
        };
        assert_eq!(cause.render(), "debug text");

        // HarnessError
        let cause = FailureCause::HarnessError("harness error".to_string());
        assert_eq!(cause.render(), "harness error");

        // Panic with message
        let cause = FailureCause::Panic(PanicCause {
            message: Some("panic msg".to_string()),
            location: None,
            backtrace: None,
        });
        assert_eq!(cause.render(), "panic msg");
        assert_eq!(cause.panic_message(), Some("panic msg"));
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn make_test(name: &str, module_path: &str) -> RegisteredTest {
        RegisteredTest {
            name: name.to_string(),
            crate_name: "mycrate".to_string(),
            module_path: module_path.to_string(),
            run: TestFunction::Sync(Arc::new(|_| Box::new(()))),
            props: TestProperties::default(),
            dependencies: None,
        }
    }

    fn make_tagged_test(name: &str, module_path: &str, tags: Vec<&str>) -> RegisteredTest {
        let mut test = make_test(name, module_path);
        test.props.tags = tags.into_iter().map(String::from).collect();
        test
    }

    fn make_args(filters: Vec<&str>, skip: Vec<&str>, exact: bool) -> Arguments {
        Arguments {
            filter: filters.into_iter().map(String::from).collect(),
            skip: skip.into_iter().map(String::from).collect(),
            exact,
            ..Default::default()
        }
    }

    fn filtered_names(args: &Arguments, tests: &[RegisteredTest]) -> Vec<String> {
        filter_registered_tests(args, tests)
            .into_iter()
            .map(|t| t.filterable_name())
            .collect()
    }

    // --- filter_test unit tests ---

    #[test]
    fn filter_test_substring_match() {
        let test = make_test("hello_world", "mod1");
        assert!(filter_test(&test, "hello", false));
        assert!(filter_test(&test, "world", false));
        assert!(filter_test(&test, "mod1::hello", false));
        assert!(!filter_test(&test, "nonexistent", false));
    }

    #[test]
    fn filter_test_exact_match() {
        let test = make_test("hello_world", "mod1");
        assert!(filter_test(&test, "mod1::hello_world", true));
        assert!(!filter_test(&test, "hello_world", true));
        assert!(!filter_test(&test, "hello", true));
    }

    #[test]
    fn filter_test_tag_match() {
        let test = make_tagged_test("t1", "mod1", vec!["fast", "unit"]);
        assert!(filter_test(&test, ":tag:fast", false));
        assert!(filter_test(&test, ":tag:unit", false));
        assert!(!filter_test(&test, ":tag:slow", false));
    }

    #[test]
    fn filter_test_tag_empty_matches_untagged() {
        let untagged = make_test("t1", "mod1");
        let tagged = make_tagged_test("t2", "mod1", vec!["fast"]);
        assert!(filter_test(&untagged, ":tag:", false));
        assert!(!filter_test(&tagged, ":tag:", false));
    }

    // --- filter_registered_tests: multiple include filters (OR semantics) ---

    #[test]
    fn no_filters_includes_all() {
        let tests = vec![make_test("a", "m"), make_test("b", "m")];
        let args = make_args(vec![], vec![], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::a", "m::b"]);
    }

    #[test]
    fn single_filter_substring() {
        let tests = vec![
            make_test("alpha", "m"),
            make_test("beta", "m"),
            make_test("alphabet", "m"),
        ];
        let args = make_args(vec!["alpha"], vec![], false);
        assert_eq!(
            filtered_names(&args, &tests),
            vec!["m::alpha", "m::alphabet"]
        );
    }

    #[test]
    fn multiple_filters_or_semantics() {
        let tests = vec![
            make_test("alpha", "m"),
            make_test("beta", "m"),
            make_test("gamma", "m"),
        ];
        let args = make_args(vec!["alpha", "gamma"], vec![], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::alpha", "m::gamma"]);
    }

    #[test]
    fn multiple_filters_exact() {
        let tests = vec![
            make_test("alpha", "m"),
            make_test("alphabet", "m"),
            make_test("beta", "m"),
        ];
        let args = make_args(vec!["m::alpha", "m::beta"], vec![], true);
        assert_eq!(filtered_names(&args, &tests), vec!["m::alpha", "m::beta"]);
    }

    // --- skip behavior ---

    #[test]
    fn skip_substring_match() {
        let tests = vec![
            make_test("fast_test", "m"),
            make_test("slow_test", "m"),
            make_test("slower_test", "m"),
        ];
        let args = make_args(vec![], vec!["slow"], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::fast_test"]);
    }

    #[test]
    fn skip_exact_match() {
        let tests = vec![make_test("slow_test", "m"), make_test("slower_test", "m")];
        let args = make_args(vec![], vec!["m::slow_test"], true);
        assert_eq!(filtered_names(&args, &tests), vec!["m::slower_test"]);
    }

    #[test]
    fn skip_with_tag() {
        let tests = vec![
            make_tagged_test("t1", "m", vec!["slow"]),
            make_tagged_test("t2", "m", vec!["fast"]),
            make_test("t3", "m"),
        ];
        let args = make_args(vec![], vec![":tag:slow"], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::t2", "m::t3"]);
    }

    // --- combined include + skip ---

    #[test]
    fn include_and_skip_combined() {
        let tests = vec![
            make_test("alpha_fast", "m"),
            make_test("alpha_slow", "m"),
            make_test("beta_fast", "m"),
        ];
        // Include anything with "alpha", but skip anything with "slow"
        let args = make_args(vec!["alpha"], vec!["slow"], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::alpha_fast"]);
    }

    #[test]
    fn skip_wins_over_include() {
        let tests = vec![make_test("target", "m")];
        // Both include and skip match the same test — skip should win
        let args = make_args(vec!["target"], vec!["target"], false);
        assert_eq!(filtered_names(&args, &tests), Vec::<String>::new());
    }

    // --- tag boolean expression syntax ---

    #[test]
    fn filter_test_tag_or_expression() {
        // `:tag:a|b` matches tests tagged with `a` OR `b`
        let test_a = make_tagged_test("t1", "m", vec!["a"]);
        let test_b = make_tagged_test("t2", "m", vec!["b"]);
        let test_c = make_tagged_test("t3", "m", vec!["c"]);
        assert!(filter_test(&test_a, ":tag:a|b", false));
        assert!(filter_test(&test_b, ":tag:a|b", false));
        assert!(!filter_test(&test_c, ":tag:a|b", false));
    }

    #[test]
    fn filter_test_tag_and_expression() {
        // `:tag:a&b` matches tests tagged with BOTH `a` AND `b`
        let test_ab = make_tagged_test("t1", "m", vec!["a", "b"]);
        let test_a = make_tagged_test("t2", "m", vec!["a"]);
        let test_b = make_tagged_test("t3", "m", vec!["b"]);
        assert!(filter_test(&test_ab, ":tag:a&b", false));
        assert!(!filter_test(&test_a, ":tag:a&b", false));
        assert!(!filter_test(&test_b, ":tag:a&b", false));
    }

    #[test]
    fn filter_test_tag_mixed_and_or() {
        // `:tag:a|b&c` means `a OR (b AND c)` — `&` has higher precedence
        let test_a = make_tagged_test("t1", "m", vec!["a"]);
        let test_bc = make_tagged_test("t2", "m", vec!["b", "c"]);
        let test_b = make_tagged_test("t3", "m", vec!["b"]);
        let test_c = make_tagged_test("t4", "m", vec!["c"]);
        let test_none = make_test("t5", "m");
        assert!(filter_test(&test_a, ":tag:a|b&c", false));
        assert!(filter_test(&test_bc, ":tag:a|b&c", false));
        assert!(!filter_test(&test_b, ":tag:a|b&c", false));
        assert!(!filter_test(&test_c, ":tag:a|b&c", false));
        assert!(!filter_test(&test_none, ":tag:a|b&c", false));
    }

    #[test]
    fn filter_test_tag_exact_flag_does_not_affect_tags() {
        // `--exact` should not change tag matching behavior
        let test = make_tagged_test("t1", "m", vec!["fast"]);
        assert!(filter_test(&test, ":tag:fast", true));
        assert!(!filter_test(&test, ":tag:slow", true));
    }

    #[test]
    fn include_by_tag_or_expression() {
        let tests = vec![
            make_tagged_test("t1", "m", vec!["unit"]),
            make_tagged_test("t2", "m", vec!["integration"]),
            make_tagged_test("t3", "m", vec!["e2e"]),
        ];
        let args = make_args(vec![":tag:unit|integration"], vec![], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::t1", "m::t2"]);
    }

    #[test]
    fn skip_by_tag_and_expression() {
        let tests = vec![
            make_tagged_test("t1", "m", vec!["slow", "network"]),
            make_tagged_test("t2", "m", vec!["slow"]),
            make_tagged_test("t3", "m", vec!["network"]),
            make_test("t4", "m"),
        ];
        // Skip only tests that are BOTH slow AND network
        let args = make_args(vec![], vec![":tag:slow&network"], false);
        assert_eq!(
            filtered_names(&args, &tests),
            vec!["m::t2", "m::t3", "m::t4"]
        );
    }

    // --- matrix auto-derived `<dim>_<case>` tags are :tag:-selectable ---
    //
    // Feature 1 makes every matrix-generated test case carry a `<dim>_<case>`
    // tag (e.g. `db_postgres`) in its `TestProperties.tags`. The `:tag:` filter
    // only ever checks `test.props.tags.contains(...)`, so once the macro
    // places the tag there the existing filter logic selects it. These tests
    // pin that contract at the runtime-filter level.

    #[test]
    fn matrix_dim_case_tag_selects_exactly_one_case() {
        // A matrix dimension `db` with cases `postgres` and `sqlite` produces
        // two generated tests, each carrying its `<dim>_<case>` auto-tag
        // alongside any explicit tag.
        let tests = vec![
            make_tagged_test("my_test_postgres", "m", vec!["db_postgres", "fast"]),
            make_tagged_test("my_test_sqlite", "m", vec!["db_sqlite", "fast"]),
        ];
        // `:tag:db_postgres` selects exactly the postgres case.
        let args = make_args(vec![":tag:db_postgres"], vec![], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::my_test_postgres"]);
    }

    #[test]
    fn matrix_dim_case_tags_select_subset_per_dimension() {
        // Cartesian product of `db` (postgres/sqlite) and `lang` (ts/rust):
        // each generated case carries the relevant subset of auto-tags.
        let tests = vec![
            make_tagged_test("combo_postgres_ts", "m", vec!["db_postgres", "lang_ts"]),
            make_tagged_test("combo_postgres_rust", "m", vec!["db_postgres", "lang_rust"]),
            make_tagged_test("combo_sqlite_ts", "m", vec!["db_sqlite", "lang_ts"]),
            make_tagged_test("combo_sqlite_rust", "m", vec!["db_sqlite", "lang_rust"]),
        ];
        // `:tag:db_postgres` selects both postgres cases regardless of lang.
        let args = make_args(vec![":tag:db_postgres"], vec![], false);
        assert_eq!(
            filtered_names(&args, &tests),
            vec!["m::combo_postgres_ts", "m::combo_postgres_rust"]
        );
        // Combine dims: `:tag:db_sqlite&lang_rust` selects exactly one case.
        let args = make_args(vec![":tag:db_sqlite&lang_rust"], vec![], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::combo_sqlite_rust"]);
    }

    #[test]
    fn matrix_auto_tag_coexists_with_explicit_tags() {
        // Explicit `#[tag(fast)]` on the test is preserved on every generated
        // case alongside the auto-derived `<dim>_<case>` tag, and both are
        // independently selectable.
        let tests = vec![
            make_tagged_test("t_postgres", "m", vec!["db_postgres", "fast"]),
            make_tagged_test("t_sqlite", "m", vec!["db_sqlite", "fast"]),
        ];
        let args = make_args(vec![":tag:fast"], vec![], false);
        assert_eq!(
            filtered_names(&args, &tests),
            vec!["m::t_postgres", "m::t_sqlite"]
        );
        let args = make_args(vec![":tag:db_sqlite"], vec![], false);
        assert_eq!(filtered_names(&args, &tests), vec!["m::t_sqlite"]);
    }
}

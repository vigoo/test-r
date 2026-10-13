use crate::internal::{CapturedOutput, FailureCause, TestResult};
use crate::stats::Summary;
use desert_rust::BinaryCodec;
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Name, NameType, ToFsName, ToNsName,
};
use std::io::{self, Read, Write};
use std::time::Duration;

/// Length-prefix width used to frame all IPC messages. A `u32` allows payloads
/// up to 4 GiB which comfortably covers Cloneable payloads such as
/// precompiled wasm components.
pub const FRAME_LEN_BYTES: usize = 4;

/// Writes a length-prefixed frame to the writer. The length is encoded as a
/// little-endian `u32`.
pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC payload size exceeds u32::MAX",
        )
    })?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload)
}

/// Reads a length-prefixed frame produced by [`write_frame`].
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_bytes = [0u8; FRAME_LEN_BYTES];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

#[cfg(feature = "tokio")]
pub async fn write_frame_async<W>(writer: &mut W, payload: &[u8]) -> io::Result<()>
where
    W: tokio::io::AsyncWriteExt + Unpin,
{
    let len = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC payload size exceeds u32::MAX",
        )
    })?;
    writer.write_all(&len.to_le_bytes()).await?;
    writer.write_all(payload).await
}

#[cfg(feature = "tokio")]
pub async fn read_frame_async<R>(reader: &mut R) -> io::Result<Vec<u8>>
where
    R: tokio::io::AsyncReadExt + Unpin,
{
    let mut len_bytes = [0u8; FRAME_LEN_BYTES];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Commands sent from the primary test runner to the spawned worker processes.
#[derive(Debug, BinaryCodec)]
pub enum IpcCommand {
    RunTest {
        name: String,
        crate_name: String,
        module_path: String,
    },
    /// Provide the wire bytes for a `Cloneable` dependency to the worker.
    /// Sent before any test that requires the dep. The `dep_id` is the
    /// dep's fully-qualified id (`{crate}::{module}::{name}`) so that
    /// same-named deps registered in different modules don't collide.
    /// Workers cache the bytes and pass them to the worker reconstructor
    /// when materializing.
    ProvideCloneable { dep_id: String, wire_bytes: Vec<u8> },
    /// Provide the descriptor bytes for a `Hosted` dependency to a worker.
    /// Same shape as [`Self::ProvideCloneable`]; on the worker side the
    /// bytes are fed to `HostedDep::from_descriptor` (via the registered
    /// worker reconstructor) instead of being treated as the dep value
    /// directly. The `dep_id` is the dep's fully-qualified id.
    ProvideHostedDescriptor { dep_id: String, wire_bytes: Vec<u8> },
    /// Phase 1C: parent's response to a worker-initiated
    /// [`IpcResponse::HostedRpcCall`]. Carries the same `request_id`
    /// echoed back so the worker's stub can match the reply to the
    /// outstanding in-flight call. `body` is `Ok(result_bytes)` if the
    /// owner-side dispatcher succeeded, or `Err(message)` if it failed
    /// (owner panic, unknown method, codec error, …).
    HostedRpcReply {
        request_id: u64,
        body: HostedRpcReplyBody,
    },
    /// Tells an IPC worker that its owning parent scheduler thread will not
    /// send any more commands. Worker lifetime is parent-controlled rather
    /// than inferred from the worker's private execution plan, whose
    /// `remaining_count` can reach zero before the parent is done dispatching.
    Shutdown,
}

/// Returns whether a test loop should stop based on its execution plan.
///
/// An in-process runner owns its scheduler and stops when that scheduler is
/// done. An IPC worker must ignore its private scheduler's completion state:
/// only an explicit [`IpcCommand::Shutdown`] from its owning parent proves
/// that no future [`IpcCommand::RunTest`] will arrive. Conversely, a parent
/// scheduler thread must retire when its owned worker reports that its private
/// plan is exhausted, even if the shared parent plan still has tests for other
/// workers.
pub(crate) fn test_loop_should_exit(
    is_ipc_worker: bool,
    execution_done: bool,
    owned_worker_exhausted: bool,
) -> bool {
    owned_worker_exhausted || (!is_ipc_worker && execution_done)
}

/// Body of a [`IpcCommand::HostedRpcReply`]. Either the serialized return
/// value of the owner's method, or a human-readable error describing why
/// dispatch failed.
#[derive(Debug, BinaryCodec)]
pub enum HostedRpcReplyBody {
    Ok { result_bytes: Vec<u8> },
    Err { message: String },
}

#[derive(Debug, BinaryCodec)]
pub enum SerializableTestResult {
    Passed {
        exec_time: Duration,
    },
    Benchmarked {
        exec_time: Duration,
        ns_iter_summ: Summary,
        mb_s: usize,
    },
    Failed {
        exec_time: Duration,
        rendered_failure_cause: String,
    },
    Ignored,
    WithAttempts {
        result: Box<SerializableTestResult>,
        attempts: Vec<SerializableTestAttempt>,
    },
}

#[derive(Debug, BinaryCodec)]
pub struct SerializableTestAttempt {
    result: SerializableTestResult,
    finish_marker: Option<String>,
}

impl SerializableTestResult {
    pub fn into_test_result(
        self,
        stdout: Vec<CapturedOutput>,
        stderr: Vec<CapturedOutput>,
    ) -> TestResult {
        if let Self::WithAttempts { result, attempts } = self {
            let mut stdout = stdout.into_iter();
            let mut stderr = stderr.into_iter();
            let mut history = Vec::new();
            let mut all_output = Vec::new();
            for attempt in attempts {
                let (out, err) = if let Some(marker) = &attempt.finish_marker {
                    (
                        take_attempt_output(&mut stdout, marker),
                        take_attempt_output(&mut stderr, marker),
                    )
                } else {
                    (Vec::new(), Vec::new())
                };
                let attempt_result = attempt.result.into_test_result(out, err);
                all_output.extend(attempt_result.captured_output().iter().cloned());
                history.push(crate::internal::TestAttempt {
                    result: Box::new(attempt_result),
                    finish_marker: None,
                });
            }
            all_output.extend(stdout);
            all_output.extend(stderr);
            all_output.sort();
            let mut result = result.into_test_result(Vec::new(), Vec::new());
            result.set_captured_output(all_output);
            result.set_attempts(history);
            return result;
        }
        let mut captured = [stdout, stderr].concat();
        captured.sort();

        let mut result: TestResult = self.into();
        result.set_captured_output(captured);
        result
    }
}

fn take_attempt_output(
    source: &mut impl Iterator<Item = CapturedOutput>,
    marker: &str,
) -> Vec<CapturedOutput> {
    let mut captured = Vec::new();
    for mut output in source.by_ref() {
        let line = match &mut output {
            CapturedOutput::Stdout { line, .. }
            | CapturedOutput::Stderr { line, .. }
            | CapturedOutput::Host { line, .. } => line,
        };
        // A test may leave a partial line before the flushed marker. Strip
        // only the unique marker suffix, retaining the test's actual bytes.
        if let Some(prefix) = line.strip_suffix(marker) {
            if !prefix.is_empty() {
                *line = prefix.to_string();
                captured.push(output);
            }
            break;
        }
        captured.push(output);
    }
    captured
}

impl From<&TestResult> for SerializableTestResult {
    fn from(result: &TestResult) -> Self {
        let final_result = Self::without_attempts(result);
        if result.attempts().is_empty() {
            final_result
        } else {
            Self::WithAttempts {
                result: Box::new(final_result),
                attempts: result
                    .attempts()
                    .iter()
                    .map(|attempt| SerializableTestAttempt {
                        result: Self::without_attempts(&attempt.result),
                        finish_marker: attempt.finish_marker.clone(),
                    })
                    .collect(),
            }
        }
    }
}

impl SerializableTestResult {
    fn without_attempts(result: &TestResult) -> Self {
        match &result {
            TestResult::Passed { exec_time, .. } => SerializableTestResult::Passed {
                exec_time: *exec_time,
            },
            TestResult::Benchmarked {
                exec_time,
                ns_iter_summ,
                mb_s,
                ..
            } => SerializableTestResult::Benchmarked {
                exec_time: *exec_time,
                ns_iter_summ: *ns_iter_summ,
                mb_s: *mb_s,
            },
            TestResult::Failed {
                exec_time, cause, ..
            } => SerializableTestResult::Failed {
                exec_time: *exec_time,
                rendered_failure_cause: cause.render(),
            },
            TestResult::Ignored { .. } => SerializableTestResult::Ignored,
        }
    }
}

impl From<SerializableTestResult> for TestResult {
    fn from(result: SerializableTestResult) -> Self {
        match result {
            SerializableTestResult::WithAttempts { .. } => {
                result.into_test_result(Vec::new(), Vec::new())
            }
            SerializableTestResult::Passed { exec_time } => TestResult::passed(exec_time),
            SerializableTestResult::Failed {
                exec_time,
                rendered_failure_cause,
            } => TestResult::failed(
                exec_time,
                FailureCause::HarnessError(rendered_failure_cause),
            ),
            SerializableTestResult::Ignored => TestResult::ignored(),
            SerializableTestResult::Benchmarked {
                exec_time,
                ns_iter_summ,
                mb_s,
            } => TestResult::benchmarked(exec_time, ns_iter_summ, mb_s),
        }
    }
}

/// A performed repetition, replayed by the parent reporter before completion.
#[derive(Debug, BinaryCodec)]
pub struct TestRetry {
    pub attempt: usize,
    pub max_attempts: usize,
    pub reason: String,
}

/// Responses sent from the spawned worker processes to the primary test
/// runner.
#[derive(Debug, BinaryCodec)]
pub enum IpcResponse {
    TestFinished {
        result: SerializableTestResult,
        finish_marker: String,
        /// Repetitions of this test only; workers drain these after each test.
        retries: Vec<TestRetry>,
        /// Whether the worker's private execution plan was exhausted while
        /// locating this test. The owning parent scheduler thread must retire
        /// after this response because the worker cannot accept another
        /// `RunTest`, but the worker process remains alive until `Shutdown`.
        worker_exhausted: bool,
    },
    /// Acknowledges a [`IpcCommand::ProvideCloneable`]. Echoes back the
    /// fully-qualified `dep_id` the command carried.
    CloneableAccepted { dep_id: String },
    /// Acknowledges a [`IpcCommand::ProvideHostedDescriptor`]. Echoes back
    /// the fully-qualified `dep_id`.
    HostedDescriptorAccepted { dep_id: String },
    /// Phase 1C: worker-initiated remote procedure call against a
    /// `HostedRpc` dep owned by the parent. The worker's stub assigns a
    /// monotonically-increasing `request_id`, serializes its method
    /// arguments into `args_bytes`, and writes this frame on the shared
    /// IPC stream. The parent's `Worker::run_test` loop dispatches the
    /// call to the right owner via `dep_id`, and responds with a matching
    /// [`IpcCommand::HostedRpcReply`].
    HostedRpcCall {
        request_id: u64,
        dep_id: String,
        method_idx: u32,
        args_bytes: Vec<u8>,
    },
}

pub fn ipc_name<'s>(name: String) -> Name<'s> {
    if GenericNamespaced::is_supported() {
        name.to_ns_name::<GenericNamespaced>()
            .expect("Invalid local socket name")
    } else {
        name.to_fs_name::<GenericFilePath>()
            .expect("Invalid local socket name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn write_then_read_round_trip_empty() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &[]).expect("write");
        let mut cursor = Cursor::new(&buf);
        let payload = read_frame(&mut cursor).expect("read");
        assert!(payload.is_empty());
    }

    #[test]
    fn write_then_read_round_trip_small() {
        let mut buf: Vec<u8> = Vec::new();
        let data = b"hello, world";
        write_frame(&mut buf, data).expect("write");
        let mut cursor = Cursor::new(&buf);
        let payload = read_frame(&mut cursor).expect("read");
        assert_eq!(payload, data);
    }

    #[test]
    fn write_then_read_round_trip_large_payload_exceeds_u16() {
        // 200 KiB — larger than the old u16 length prefix could express.
        let mut data = vec![0u8; 200 * 1024];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &data).expect("write");
        assert_eq!(buf.len(), FRAME_LEN_BYTES + data.len());
        let mut cursor = Cursor::new(&buf);
        let payload = read_frame(&mut cursor).expect("read");
        assert_eq!(payload.len(), data.len());
        assert_eq!(payload, data);
    }

    #[test]
    fn read_frame_propagates_eof() {
        let buf: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(&buf);
        let err = read_frame(&mut cursor).expect_err("must fail on empty");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn worker_lifetime_and_parent_scheduler_capacity_are_distinct() {
        assert!(test_loop_should_exit(false, true, false));
        assert!(!test_loop_should_exit(true, true, false));
        assert!(!test_loop_should_exit(true, false, false));
        assert!(test_loop_should_exit(false, false, true));
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn async_round_trip_large_payload_exceeds_u16() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut data = vec![0u8; 200 * 1024];
            for (i, b) in data.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
            let mut buf: Vec<u8> = Vec::new();
            write_frame_async(&mut buf, &data).await.expect("write");
            assert_eq!(buf.len(), FRAME_LEN_BYTES + data.len());
            let mut slice: &[u8] = &buf;
            let payload = read_frame_async(&mut slice).await.expect("read");
            assert_eq!(payload, data);
        });
    }
}

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
}

impl SerializableTestResult {
    pub fn into_test_result(
        self,
        stdout: Vec<CapturedOutput>,
        stderr: Vec<CapturedOutput>,
    ) -> TestResult {
        let mut captured = [stdout, stderr].concat();
        captured.sort();

        let mut result: TestResult = self.into();
        result.set_captured_output(captured);
        result
    }
}

impl From<&TestResult> for SerializableTestResult {
    fn from(result: &TestResult) -> Self {
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

/// Responses sent from the spawned worker processes to the primary test
/// runner.
#[derive(Debug, BinaryCodec)]
pub enum IpcResponse {
    TestFinished {
        result: SerializableTestResult,
        finish_marker: String,
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

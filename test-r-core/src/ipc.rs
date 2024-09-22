use crate::internal::{CapturedOutput, TestResult};
use bincode::{Decode, Encode};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Name, NameType, ToFsName, ToNsName,
};

/// Commands sent from the primary test runner to the spawned worker processes.
#[derive(Debug, Encode, Decode)]
pub enum IpcCommand {
    RunTest {
        name: String,
        crate_name: String,
        module_path: String,
    },
}

#[derive(Debug, Encode, Decode)]
pub enum SerializableTestResult {
    Passed,
    Failed { panic: String },
    Ignored,
}

impl SerializableTestResult {
    pub fn into_test_result(
        self,
        stdout: Vec<CapturedOutput>,
        stderr: Vec<CapturedOutput>,
    ) -> TestResult {
        let mut captured = vec![stdout, stderr].concat();
        captured.sort();

        let mut result: TestResult = self.into();
        result.set_captured_output(captured);
        result
    }
}

impl From<&TestResult> for SerializableTestResult {
    fn from(result: &TestResult) -> Self {
        match &result {
            TestResult::Passed { .. } => SerializableTestResult::Passed,
            TestResult::Failed { .. } => SerializableTestResult::Failed {
                panic: result.failure_message().unwrap_or_default().to_string(),
            },
            TestResult::Ignored { .. } => SerializableTestResult::Ignored,
        }
    }
}

impl From<SerializableTestResult> for TestResult {
    fn from(result: SerializableTestResult) -> Self {
        match result {
            SerializableTestResult::Passed => TestResult::passed(),
            SerializableTestResult::Failed { panic } => TestResult::failed(Box::new(panic)),
            SerializableTestResult::Ignored => TestResult::ignored(),
        }
    }
}

/// Responses sent from the spawned worker processes to the primary test runner.
#[derive(Debug, Encode, Decode)]
pub enum IpcResponse {
    TestFinished { result: SerializableTestResult },
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

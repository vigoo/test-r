use std::sync::Mutex;

pub struct RegisteredTest {
    pub name: String,
    pub module_path: String,
    pub run: Box<dyn Fn() + Send>,
}

pub static REGISTERED_TESTS: Mutex<Vec<RegisteredTest>> = Mutex::new(Vec::new());

extern crate alloc;

pub use test_r_macro::test;
pub use test_r_macro::uses_test_r as enable;

pub mod core {
    pub use test_r_core::internal::TestFunction;
    pub use test_r_core::*;

    pub fn register_test(name: &str, module_path: &str, is_ignored: bool, run: TestFunction) {
        let (crate_name, module_path) = crate::core::split_module_path(module_path);

        internal::REGISTERED_TESTS
            .lock()
            .unwrap()
            .push(internal::RegisteredTest {
                name: name.to_string(),
                crate_name,
                module_path,
                is_ignored,
                run,
            });
    }

    fn split_module_path(module_path: &str) -> (String, String) {
        let (crate_name, module_path) =
            if let Some((crate_name, module_path)) = module_path.split_once("::") {
                (crate_name.to_string(), module_path.to_string())
            } else {
                (module_path.to_string(), String::new())
            };
        (crate_name, module_path)
    }
}

pub use ::ctor;

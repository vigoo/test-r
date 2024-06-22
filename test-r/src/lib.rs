pub use test_r_macro::test;
pub use test_r_macro::{uses_test_r as enable};

pub mod core {
    pub use test_r_core::*;

    pub fn register_test(name: &str, module_path: &str, run: Box<dyn Fn() + Send>) {
        println!("Registering test {name}");
        test_r_core::internal::REGISTERED_TESTS.lock().unwrap()
            .push(test_r_core::internal::RegisteredTest {
                name: name.to_string(),
                module_path: module_path.to_string(),
                run,
            });
    }
}

pub use ::ctor;
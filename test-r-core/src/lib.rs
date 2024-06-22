use clap::CommandFactory;

pub mod args;
pub mod internal;

pub fn test_runner() {
    let args = args::Arguments::from_args();
    println!("Args: {args:?}");

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    for registered_test in registered_tests.iter() {
        println!("Registered test: {}::{}", registered_test.module_path, registered_test.name);
        // TODO: filter

        (registered_test.run)();
    }
}
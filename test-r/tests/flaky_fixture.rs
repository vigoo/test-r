use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use test_r::core::{DynamicTestRegistration, FlakinessControl, TestProperties};

test_r::enable!();

#[test_r::test]
#[test_r::flaky(2)]
#[ignore]
fn attribute_sync() -> Result<(), String> {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let attempt = CALLS.fetch_add(1, Ordering::SeqCst) + 1;
    println!("ATTEMPT {attempt}");
    if attempt == 1 {
        Err("transient error".to_string())
    } else {
        Ok(())
    }
}

#[cfg(feature = "tokio")]
#[test_r::test]
#[test_r::flaky(2)]
#[ignore]
async fn attribute_async() -> Result<(), String> {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let attempt = CALLS.fetch_add(1, Ordering::SeqCst) + 1;
    println!("ATTEMPT {attempt}");
    if attempt == 1 {
        Err("transient error".to_string())
    } else {
        Ok(())
    }
}

#[test_r::test_gen]
fn generated(r: &mut DynamicTestRegistration) {
    for scenario in ["transient", "persistent", "success", "unmarked", "panic"] {
        let props = TestProperties {
            is_ignored: true,
            flakiness_control: if scenario == "unmarked" {
                FlakinessControl::None
            } else {
                FlakinessControl::RetryKnownFlaky(3)
            },
            ..TestProperties::unit_test()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let run = move |_| -> Result<(), std::io::Error> {
            let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
            println!("ATTEMPT {attempt}");
            match scenario {
                "success" => Ok(()),
                "transient" if attempt == 2 => Ok(()),
                "panic" if attempt == 2 => Ok(()),
                "panic" => panic!("transient panic"),
                _ => Err(std::io::Error::other(format!("error on attempt {attempt}"))),
            }
        };
        r.add_sync_test(format!("sync_{scenario}"), props.clone(), None, run.clone());
        #[cfg(feature = "tokio")]
        r.add_async_test(format!("async_{scenario}"), props, None, move |deps| {
            let run = run.clone();
            Box::pin(async move { run(deps) })
        });
    }

    #[cfg(feature = "tokio")]
    r.add_async_test(
        "async_timeout",
        TestProperties {
            is_ignored: true,
            timeout: Some(std::time::Duration::from_millis(10)),
            flakiness_control: FlakinessControl::RetryKnownFlaky(3),
            ..TestProperties::unit_test()
        },
        None,
        |_| {
            Box::pin(async {
                println!("ATTEMPT 1");
                std::future::pending::<()>().await;
            })
        },
    );
}

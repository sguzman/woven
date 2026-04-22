use std::env;

use woven::config;
use woven::kernel::KernelSession;

#[test]
fn kernel_evaluates_simple_expression() {
    if env::var("WOVEN_TEST_KERNEL").ok().as_deref() != Some("1") {
        eprintln!("skipping: set WOVEN_TEST_KERNEL=1 to run");
        return;
    }

    let cfg = config::load().expect("load config");
    let mut kernel = KernelSession::new(&cfg.kernel).expect("start kernel");
    let out = kernel.evaluate(1, "1+1").expect("eval");

    assert!(
        out.output_text.contains('2'),
        "output was: {}",
        out.output_text
    );
}

#[test]
fn kernel_evaluates_derivative_expression() {
    if env::var("WOVEN_TEST_KERNEL").ok().as_deref() != Some("1") {
        eprintln!("skipping: set WOVEN_TEST_KERNEL=1 to run");
        return;
    }

    let cfg = config::load().expect("load config");
    let mut kernel = KernelSession::new(&cfg.kernel).expect("start kernel");
    let out = kernel.evaluate(2, "Derivative[1][Sin][x]").expect("eval");

    assert!(
        out.output_text.contains("Cos")
            || out.output_text.contains("cos")
            || out.output_text.contains("Cos[x]"),
        "output was: {}",
        out.output_text
    );
}

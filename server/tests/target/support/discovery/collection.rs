pub fn run(tests: &[&dyn Fn()]) {
    assert!(!tests.is_empty(), "compiler must collect at least one case");
    for test in tests {
        test();
    }
}

#[test]
fn console_stops_at_the_first_byte_the_output_cannot_accept() {
    use core::fmt::Write;
    use core::sync::atomic::{AtomicUsize, Ordering};

    static WRITES: AtomicUsize = AtomicUsize::new(0);
    let mut writer = super::Console {
        write_byte: |byte| {
            let index = WRITES.fetch_add(1, Ordering::Relaxed);
            assert_eq!(byte, "圜".as_bytes()[index]);
            index == 0
        },
    };
    assert!(writer.write_str("圜").is_err());
    assert_eq!(WRITES.load(Ordering::Relaxed), 2);
}

#[path = "fixture.rs"]
mod fixture;

use fixture::Fixture;

#[test]
fn keeps_the_trusted_dtb_and_memory_descriptions_on_one_view() {
    #[repr(align(8))]
    struct Storage([u8; 4096]);

    let mut fixture = Fixture::root();
    fixture.node(b"memory@80000000");
    fixture.property(b"device_type", b"memory\0");
    fixture.cells(b"reg", &[0x8000_0000, 0x4000]);
    fixture.end();
    let source = fixture.finish();
    let mut storage = Storage([0; 4096]);
    storage.0[..source.len()].copy_from_slice(&source);
    let start = storage.0.as_ptr().addr();
    let mut memory = None;
    let tree = unsafe {
        super::read(
            storage.0.as_ptr().cast(),
            start..start + storage.0.len(),
            0..0,
            &mut memory,
        )
    }
    .unwrap();
    assert_eq!(tree.bytes().as_ptr().addr(), start);
    assert_eq!(tree.bytes().len(), source.len());
    let mut reservations_only = None;
    super::describe_reservations(tree, &mut reservations_only).unwrap();
    assert_eq!(reservations_only.unwrap().ram().len(), 0);

    assert_eq!(
        memory.unwrap().ram().next(),
        Some(
            tree.find_path(b"/memory@80000000")
                .unwrap()
                .unwrap()
                .register(0)
                .unwrap()
        )
    );
}

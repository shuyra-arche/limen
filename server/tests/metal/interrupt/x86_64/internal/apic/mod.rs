use limen_treaty::model::{ByteLength, PhysicalAddress};

use super::{
    Base, Observation, Probe,
    launch::{Command, Vector},
    registers::{Destination, idle, integrated},
};

#[test]
fn base_requires_actual_legacy_enabled_mode() {
    let base = Base::decode(0xfee0_0900).unwrap();
    assert_eq!(base.physical.start(), PhysicalAddress::new(0xfee0_0000));
    assert_eq!(base.physical.length(), ByteLength::new(4096));
    assert!(base.major);
    assert!(!Base::decode(0xfee0_0800).unwrap().major);
    for bits in [0, 0xfee0_0100, 0xfee0_0d00, 0xfee0_0901, 0x900] {
        assert!(Base::decode(bits).is_none());
    }
    assert!(integrated(0x0005_0014));
    assert!(!integrated(0x0005_000f));
    assert!(!integrated(0x0005_0016));
}

#[test]
fn startup_command_and_observation_are_not_the_same_encoding() {
    assert_eq!(Command::Init.bits(), 0x4500);
    let vector = Vector::new(PhysicalAddress::new(0x9f000)).unwrap();
    assert_eq!(Command::Startup(vector).bits(), 0x469f);
    assert_eq!(Destination::new(0xfe).unwrap().bits(), 0xfe00_0000);
    for invalid in [0, 1, 0x100000, u64::MAX] {
        assert!(Vector::new(PhysicalAddress::new(invalid)).is_none());
    }
    assert!(Destination::new(255).is_none());
    assert!(Destination::new(256).is_none());
    assert!(idle(0x4500));
    assert!(!idle(0x5500));
}

#[test]
fn native_observation_source_is_read_again_for_each_scope_check() {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static READS: AtomicUsize = AtomicUsize::new(0);
    fn observe() -> Option<Observation> {
        match READS.fetch_add(1, Ordering::Relaxed) {
            0 => Some(Observation {
                bits: 0xfee0_0900,
                hart: 7,
            }),
            1 => Some(Observation {
                bits: 0xfee0_0900,
                hart: 8,
            }),
            2 => Some(Observation {
                bits: 0xfee0_0d00,
                hart: 7,
            }),
            _ => None,
        }
    }

    // Only native observation data is controlled; no device access is made.
    let probe = Probe { read: observe };
    assert!(probe.base_for(7).is_some());
    assert!(probe.base_for(7).is_none());
    assert!(probe.base_for(7).is_none());
    assert!(probe.base_for(7).is_none());
    assert_eq!(READS.load(Ordering::Relaxed), 4);
}

use super::{Error, Profile, translation::Format};
use limen_treaty::model::PhysicalAddress;

#[test]
fn stage_two_requires_the_aarch64_format_and_4k_granule() {
    let profile = Profile {
        stage_two: true,
        granule_4k: true,
        enabled: false,
        aarch64_tables: true,
    };
    let format = Format::new(profile).expect("supported profile");
    assert_eq!(
        format.stream(PhysicalAddress::new(0x2000), 2).unwrap()[3],
        0x2000
    );
    assert_eq!(
        format.branch(PhysicalAddress::new(1 << 32)),
        Err(Error::Window)
    );
    for profile in [
        Profile {
            stage_two: false,
            ..profile
        },
        Profile {
            granule_4k: false,
            ..profile
        },
        Profile {
            aarch64_tables: false,
            ..profile
        },
    ] {
        assert!(matches!(Format::new(profile), Err(Error::Rejected)));
    }
}

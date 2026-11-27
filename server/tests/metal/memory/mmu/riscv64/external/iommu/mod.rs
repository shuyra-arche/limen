use super::{Directory, DirectoryWord, Error};
use limen_treaty::model::PhysicalAddress;

#[test]
fn directory_readback_checks_the_installed_root() {
    let previous = DirectoryWord((1 << 54) | 1);
    let target = match previous.one(PhysicalAddress::new(0x9ab_000), 48) {
        Ok(target) => target,
        Err(error) => panic!("valid directory address: {error:?}"),
    };
    assert_eq!(target.mode(), Directory::One);
    assert_eq!(target.root(), Some(PhysicalAddress::new(0x9ab_000)));
    assert_eq!(target.0 >> 54, previous.0 >> 54);
    assert!(!DirectoryWord(target.0 ^ (1 << 10)).confirms(target));
    assert!(target.confirms(target));

    let off = target.off();
    assert_eq!(off.mode(), Directory::Off);
    assert_eq!(off.0 & !0xf, target.0 & !0xf);
    assert!(DirectoryWord(off.0 ^ (1 << 10)).confirms(off));
    assert!(!off.confirms(target));
}

#[test]
fn directory_rejects_unaligned_and_out_of_range_roots() {
    let previous = DirectoryWord(0);
    assert!(matches!(
        previous.one(PhysicalAddress::new(0x123), 48),
        Err(Error::Address)
    ));
    assert!(matches!(
        previous.one(PhysicalAddress::new(1 << 38), 38),
        Err(Error::Address)
    ));
    assert!(matches!(
        previous.one(PhysicalAddress::new(1 << 56), 64),
        Err(Error::Address)
    ));
}

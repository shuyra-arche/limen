use super::*;

const RAW: u32 = 0x000a_0001;

#[test]
fn the_exact_claim_survives_mask_readback_and_slot_reuse() {
    let mut reception = Reception::new();
    reception.begin().unwrap();
    assert_eq!(reception.uncertain(), Some((1, None)));
    assert_eq!(reception.begin(), Err(Failure::Busy));

    let (first, _) = reception
        .observe(RAW, |_| Ok(Source::Enabled(Mode::LevelHigh)))
        .unwrap()
        .unwrap();
    assert_eq!(reception.uncertain(), Some((1, Some(RAW))));
    assert_eq!(reception.current(), None);
    assert_eq!(reception.settle(first), Err(Failure::Busy));
    assert_eq!(reception.observe(0, |_| unreachable!()), Err(Failure::Busy));
    reception.confirm_mask(first).unwrap();
    assert_eq!(reception.current(), Some(first));
    reception.settle(first).unwrap();

    reception.begin().unwrap();
    let (second, _) = reception
        .observe(RAW, |_| Ok(Source::Enabled(Mode::LevelHigh)))
        .unwrap()
        .unwrap();
    assert_eq!(second.round, first.round + 1);
    assert_eq!(reception.confirm_mask(first), Err(Failure::Busy));
    reception.confirm_mask(second).unwrap();
    assert_eq!(reception.settle(first), Err(Failure::Busy));
    assert_eq!(reception.current(), Some(second));
}

#[test]
fn an_unexpected_claim_retains_its_raw_word_and_blocks_a_second_read() {
    let mut reception = Reception::new();
    reception.begin().unwrap();
    assert_eq!(reception.observe(0, |_| unreachable!()), Ok(None));

    reception.begin().unwrap();
    assert_eq!(
        reception.observe(RAW, |_| Ok(Source::Vacant)),
        Err(Failure::Unconfirmed)
    );
    assert_eq!(reception.uncertain(), Some((2, Some(RAW))));
    assert_eq!(reception.begin(), Err(Failure::Busy));
    assert_eq!(reception.observe(0, |_| unreachable!()), Err(Failure::Busy));
}

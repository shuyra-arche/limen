use super::{Error, State};

#[test]
fn a_stopping_or_failed_queue_cannot_admit_a_directory_edit() {
    assert_eq!(State::Starting.admits_command(), Ok(()));
    assert_eq!(State::Ready.admits_command(), Ok(()));
    assert_eq!(State::Shutting.admits_command(), Err(Error::Busy));
    assert_eq!(
        State::Failed(None).admits_command(),
        Err(Error::Unconfirmed)
    );
    assert_eq!(State::Shut.admits_command(), Err(Error::Active));
}

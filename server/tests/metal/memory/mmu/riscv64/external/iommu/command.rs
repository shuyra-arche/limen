use super::super::Context;
use super::{Binding, CommandIdentity, Effect, PhysicalAddress, Refresh, Sequence};

#[test]
fn binding_publication_and_retirement_have_disjoint_ids() {
    let mut sequence = Sequence::FRESH;
    let binding = Binding::new(
        PhysicalAddress::new(0x1000),
        sequence.next().unwrap(),
        Context {
            device: 7,
            root: PhysicalAddress::new(0x3000),
            pscid: 1,
        },
    );
    assert_eq!(
        Effect::Bound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Ordinary(1)),
    );
    assert_eq!(
        Effect::Refreshed(binding, Refresh::FirstStage).command_identity(&mut sequence),
        Ok(CommandIdentity::Ordinary(2)),
    );
    assert_eq!(
        Effect::Unbound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Retirement(1)),
    );
    assert_eq!(sequence.next(), Ok(3));
}

#[test]
fn a_faulted_second_fence_keeps_the_original_effect() {
    use super::super::{DeviceOrder, DeviceWindow, Error};
    use super::{Commands, Page, Queue, Registers};
    use core::cell::UnsafeCell;
    use limen_treaty::model::{ByteLength, InternalAddress};
    use std::boxed::Box;

    const HEAD: usize = 0x20;
    const TAIL: usize = 0x24;
    const STATUS: usize = 0x48;
    const ACTIVE: u32 = 1 << 16;
    const ENABLE: u32 = 1;
    const FAULT: u32 = 1 << 8;

    let registers = Box::leak(Box::new(UnsafeCell::new([0u64; 10])));
    let base = registers.get().cast::<u8>();
    let device_update =
        |offset, word: u32| unsafe { base.add(offset).cast::<u32>().write_volatile(word.to_le()) };
    let device_word =
        |offset| unsafe { u32::from_le(base.add(offset).cast::<u32>().read_volatile()) };

    device_update(0, 0x10);
    let controller = PhysicalAddress::new(0x1000);
    let mut window = unsafe {
        DeviceWindow::admit(
            controller.range(ByteLength::new(0x50)).unwrap(),
            InternalAddress::new(base as u64),
        )
    }
    .unwrap();
    let (mut registers, _) = unsafe { Registers::admit(&mut window, DeviceOrder(|| {})) }.unwrap();
    let mut sequence = Sequence::FRESH;
    let generation = sequence.next().unwrap();
    let page = Box::leak(Box::new(Page::EMPTY));
    let (window, _) = registers.queue_window();
    let queue =
        unsafe { Queue::prepare(window, page, PhysicalAddress::new(0x2000), generation) }.unwrap();
    let mut commands = Commands::new(queue);
    commands.start(&mut registers).unwrap();
    device_update(STATUS, ENABLE | ACTIVE);

    let binding = Binding::new(
        controller,
        sequence.next().unwrap(),
        Context {
            device: 7,
            root: PhysicalAddress::new(0x3000),
            pscid: 1,
        },
    );
    let effect = Effect::Unbound(binding);
    let command = commands
        .slot()
        .unwrap()
        .reserve(controller, effect, &mut sequence)
        .unwrap();

    assert_eq!(commands.observe(command, &mut registers), Err(Error::Busy));
    assert_eq!(commands.observe(command, &mut registers), Err(Error::Busy));
    assert_eq!(device_word(TAIL), 2);

    device_update(HEAD, 2);
    assert_eq!(commands.observe(command, &mut registers), Err(Error::Busy));
    assert_eq!(device_word(TAIL), 2);
    assert_eq!(commands.observe(command, &mut registers), Err(Error::Busy));
    assert_eq!(device_word(TAIL), 4);

    device_update(HEAD, 4);
    device_update(STATUS, ENABLE | ACTIVE | FAULT);
    assert_eq!(
        commands.observe(command, &mut registers),
        Err(Error::Rejected)
    );
    assert_eq!(
        commands.observe(command, &mut registers),
        Err(Error::Unconfirmed)
    );
    assert_eq!(commands.command(), Some(command));
    assert_eq!(commands.effect(), Some(effect));
    assert_eq!(device_word(TAIL), 4);
}

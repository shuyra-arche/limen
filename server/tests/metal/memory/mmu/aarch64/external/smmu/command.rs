use super::super::Context;
use super::{
    Binding, CommandIdentity, Commands, Effect, Error, Invalidate, PhysicalAddress, Refresh,
    Sequence,
};

fn original(controller: u64) -> Binding {
    Binding {
        controller: PhysicalAddress::new(controller),
        generation: 7,
        context: Context {
            stream: 12,
            root: PhysicalAddress::new(0x8000),
            vmid: 2,
        },
    }
}

#[test]
fn a_second_command_cannot_replace_the_original_effect() {
    let binding = original(0x1000);
    let mut sequence = Sequence::FRESH;
    let mut commands = Commands::new();
    let effect = Effect::Unbound(binding);
    let command = commands
        .reserve(binding.controller, 4, effect, &mut sequence)
        .unwrap();

    let other = Effect::Bound(original(0x2000));
    assert_eq!(
        commands.reserve(binding.controller, 4, other, &mut sequence),
        Err(Error::Busy)
    );
    assert_eq!(commands.command(), Some(command));
    assert_eq!(commands.effect(), Some(effect));
}

#[test]
fn retirement_requires_both_original_cache_scopes() {
    let binding = original(0x1000);
    let effect = Effect::Unbound(binding);
    let configuration = Invalidate::Stream(binding.context.stream);
    let translation = Invalidate::StageTwo(binding.context.vmid);
    let first = effect.first();
    assert_eq!(first.operation(effect.binding()), Ok(configuration));
    let second = effect.next(first).unwrap();
    assert_eq!(second.operation(effect.binding()), Ok(translation));
    assert_eq!(effect.next(second), None);

    let refresh = Effect::Refreshed(binding, Refresh::StageTwo);
    let first = refresh.first();
    assert_eq!(first.operation(refresh.binding()), Ok(translation));
    assert_eq!(refresh.next(first), None);
}

#[test]
fn a_prepared_binding_keeps_its_original_retirement_at_sequence_exhaustion() {
    let mut sequence = Sequence {
        number: u64::MAX - 2,
        ..Sequence::FRESH
    };
    let binding = Binding {
        controller: PhysicalAddress::new(0x1000),
        generation: sequence.prepare_binding().unwrap(),
        context: Context {
            stream: 7,
            root: PhysicalAddress::new(0x3000),
            vmid: 1,
        },
    };
    assert_eq!(sequence.prepare_binding(), Err(Error::Exhausted));
    assert_eq!(sequence.next(), Ok(u64::MAX)); // First queue.
    assert_eq!(sequence.next_available(), Err(Error::Exhausted));
    assert_eq!(
        Effect::Refreshed(binding, Refresh::StageTwo).command_identity(&mut sequence),
        Err(Error::Exhausted),
    );
    assert_eq!(
        Effect::Unbound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Retirement(u64::MAX - 1)),
    );
}

#[test]
fn final_binding_installation_and_its_retirement_have_disjoint_ids() {
    let mut sequence = Sequence {
        number: u64::MAX - 1,
        ..Sequence::FRESH
    };
    let binding = Binding {
        controller: PhysicalAddress::new(0x1000),
        generation: sequence.next().unwrap(),
        context: Context {
            stream: 7,
            root: PhysicalAddress::new(0x3000),
            vmid: 1,
        },
    };
    assert_eq!(
        Effect::Bound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Ordinary(u64::MAX)),
    );
    assert_eq!(
        Effect::Unbound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Retirement(u64::MAX)),
    );
}

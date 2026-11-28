//! One fixed-local Fund, sharing its original comparator with Sign.

use core::{
    cell::{Cell, UnsafeCell},
    num::NonZeroUsize,
    ops::Range,
    ptr,
    time::Duration,
};

use super::{
    Current, DeadlineError, DeadlineProgress, Entry, Error, Execution, KernelBinding, Start,
};
use crate::{
    basis::{
        room::{LeaveDestination, Occupancy, Queue},
        sign::Sign,
        unit::{End, UnitId},
    },
    metal::{
        chrono::treaty::{Deadline, LocalTiming, Meter, MeterReceipt},
        interrupt::{
            riscv64::internal::timer::{Failure, Timer},
            treaty::AbsoluteTimer,
        },
        processor::treaty::{self, Context, TimerInterrupt},
    },
};

pub use crate::basis::room::{ChargeSettlement, PeriodKey, PeriodRecord, PeriodSettlement};

pub struct FundConfig<'clock> {
    /// First certified ordinary window. The supplied record count fixes the
    /// number of contiguous equal-width windows at this original quota.
    pub opens: Deadline<'clock>,
    pub shuts: Deadline<'clock>,
    pub quota: Duration,
    pub ceiling: Duration,
    pub stop: crate::metal::chrono::treaty::StopBounds,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FundError {
    Execution(Error),
    Timer(Failure),
}

/// Rejection leaves the original comparator and its effects with the caller.
pub struct FundEstablishFailure<'clock> {
    pub timer: Timer<'clock>,
    pub periods: &'static mut [PeriodRecord],
    pub reason: FundError,
}

impl core::fmt::Debug for FundEstablishFailure<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl From<Error> for FundError {
    fn from(value: Error) -> Self {
        Self::Execution(value)
    }
}

impl From<Failure> for FundError {
    fn from(value: Failure) -> Self {
        Self::Timer(value)
    }
}

impl From<DeadlineError<Failure>> for FundError {
    fn from(value: DeadlineError<Failure>) -> Self {
        match value {
            DeadlineError::Execution(error) => Self::Execution(error),
            DeadlineError::Timer(error) => Self::Timer(error),
        }
    }
}

#[derive(Clone, Copy)]
struct Active<'clock> {
    occupancy: Occupancy,
    context: &'static Context,
    meter: Meter<'clock, Occupancy>,
    result: Option<u64>,
}

#[derive(Clone, Copy)]
struct Closed {
    receipt: MeterReceipt<Occupancy>,
    result: Option<u64>,
}

#[derive(Clone, Copy)]
enum State<'clock> {
    Idle,
    Armed(Occupancy),
    Measuring(Active<'clock>),
    Closed(Closed),
}

/// One original Timer and a finite ordinary schedule on an isolated processor.
/// Sign owns the absolute waits; Room owns the Periods and Charges.
pub struct Fund<'clock> {
    timing: LocalTiming<'clock>,
    config: FundConfig<'clock>,
    windows: NonZeroUsize,
    // Until Attach succeeds, this is the original unconsumed Room supply.
    periods: Cell<Option<&'static mut [PeriodRecord]>>,
    timer: UnsafeCell<Timer<'clock>>,
    // The typed Sign is supplied explicitly by the resident dispatcher.
    // Its storage is borrowed for the original clock lifetime at admission.
    sign: usize,
    execution: Cell<Option<usize>>,
    state: Cell<State<'clock>>,
}

// Timer and metering state are accessed only after the original local entry
// is checked, with delivery masked. No hardware or clock reference escapes.
unsafe impl Sync for Fund<'_> {}

impl<'clock> Fund<'clock> {
    /// Bind the original local clock, resident Sign and settled comparator.
    ///
    /// # Safety
    /// The trusted platform must reserve this processor's funded execution
    /// supply and comparator for every supplied window and connect the original Timer
    /// Hook to this resident Fund. Only this local dispatcher may service the
    /// bound Sign or program its comparator; the Sign must remain resident.
    /// The caller certifies the whole finite schedule, including its quota
    /// per window and the resources to service Sign across boundaries.
    /// Supply timing bounds for this exact machine, IRQ configuration,
    /// permitted native spans and Rust entry and exit tails.
    /// Account for programming, delivery and masking,
    /// the machine switch, and the final meter read. A nominal frequency
    /// or observed emulator timing does not establish those bounds.
    pub unsafe fn new<const N: usize>(
        timer: Timer<'clock>,
        timing: LocalTiming<'clock>,
        config: FundConfig<'clock>,
        sign: &'clock Sign<'clock, N>,
        periods: &'static mut [PeriodRecord],
    ) -> Result<Self, FundEstablishFailure<'clock>> {
        let windows = NonZeroUsize::new(periods.len());
        let last = windows.and_then(|windows| Self::window_for(&timing, &config, windows));
        let result = if !ptr::eq(timer.clock(), timing.clock())
            || !ptr::eq(sign.clock, timing.clock())
            || config.quota < config.ceiling
            || config.ceiling.is_zero()
            || !PeriodRecord::accepts(periods)
            || !timing.window_valid(config.opens, config.shuts)
            || !last.is_some_and(|(_, shuts)| timing.window_valid(config.opens, shuts))
        {
            Err(Error::Timing.into())
        } else {
            match timer.confirmed_deadline() {
                Ok(None) => Ok(()),
                Ok(Some(_)) => Err(Error::Busy.into()),
                Err(error) => Err(error.into()),
            }
        };
        if let Err(reason) = result {
            return Err(FundEstablishFailure {
                timer,
                periods,
                reason,
            });
        }
        Ok(Self {
            timing,
            config,
            windows: windows.expect("checked window count"),
            periods: Cell::new(Some(periods)),
            timer: UnsafeCell::new(timer),
            sign: ptr::from_ref(sign).addr(),
            execution: Cell::new(None),
            state: Cell::new(State::Idle),
        })
    }

    fn window_for(
        timing: &LocalTiming<'clock>,
        config: &FundConfig<'clock>,
        window: NonZeroUsize,
    ) -> Option<(Deadline<'clock>, Deadline<'clock>)> {
        let clock = timing.clock();
        let first = clock.count_for(config.opens)?;
        let width = clock.count_for(config.shuts)?.checked_sub(first)?;
        if width == 0 {
            return None;
        }
        let ordinal = u64::try_from(window.get()).ok()?;
        let shuts = first.checked_add(width.checked_mul(ordinal)?)?;
        if shuts == u64::MAX {
            return None;
        }
        let opens = shuts.checked_sub(width)?;
        Some((clock.deadline(opens), clock.deadline(shuts)))
    }

    fn window(&self, key: PeriodKey) -> Result<(Deadline<'clock>, Deadline<'clock>), Error> {
        if !key.belongs_to(self.id()) || key.window() > self.windows {
            return Err(Error::Gone);
        }
        Self::window_for(&self.timing, &self.config, key.window()).ok_or(Error::Timing)
    }

    fn id(&self) -> usize {
        ptr::from_ref(self).addr()
    }

    fn check_local(&self) -> Result<(), Error> {
        if !treaty::budget_ready()
            || treaty::try_current_processor().map(|id| id.get())
                != Some(self.timing.clock().processor())
        {
            return Err(Error::Protection);
        }
        Ok(())
    }

    fn check_execution(&self, execution: &Execution) -> Result<(), Error> {
        if self.execution.get() != Some(ptr::from_ref(execution).addr()) {
            return Err(Error::Gone);
        }
        Ok(())
    }

    fn check_sign<const N: usize>(&self, sign: &Sign<'_, N>) -> Result<(), Error> {
        if self.sign != ptr::from_ref(sign).addr() || !ptr::eq(sign.clock, self.timing.clock()) {
            return Err(Error::Timing);
        }
        Ok(())
    }

    pub(super) fn check_wait<const N: usize>(
        &self,
        current: &Current,
        sign: &Sign<'_, N>,
        deadline: Deadline<'_>,
    ) -> Result<(), Error> {
        self.check_local()?;
        self.check_execution(current.execution)?;
        self.check_sign(sign)?;
        let State::Measuring(active) = self.state.get() else {
            return Err(Error::State);
        };
        let (_, local) = current.execution.local()?;
        if active.occupancy.unit() != current.unit || local.running() != Some(active.occupancy) {
            return Err(Error::Gone);
        }
        // Shared dispatch supplies permit other processors to contend for
        // the Unit/Room Turn. Until a bounded Arm/exit protocol is admitted,
        // do not start that masked operation during a shared Fund occupancy.
        // A single supplied dispatcher is not itself a timing certificate.
        if current.execution.dispatchers.len() != 1 {
            return Err(Error::Unsupported);
        }
        let count = sign.clock.count_for(deadline).ok_or(Error::Timing)?;
        let (_, last) =
            Self::window_for(&self.timing, &self.config, self.windows).ok_or(Error::Timing)?;
        let shuts = sign.clock.count_for(last).ok_or(Error::Timing)?;
        if count >= shuts {
            return Err(Error::Timing);
        }
        Ok(())
    }

    fn confirm_timer(&self) -> Result<(), FundError> {
        let timer = unsafe { &mut *self.timer.get() };
        if timer.pending_effect()?.is_some() {
            timer.confirm_effect()?;
        }
        // A live comparison can still cover an original Sign wait.
        Ok(())
    }

    /// Receive this Fund's original local Timer interrupt. The native
    /// return gate receives the same live Occupancy before leaving the Unit.
    pub fn on_timer(
        &self,
        execution: &Execution,
        interrupt: TimerInterrupt<'_>,
    ) -> Result<bool, FundError> {
        if interrupt.processor().get() != self.timing.clock().processor() {
            return Err(Error::Protection.into());
        }
        self.check_execution(execution)?;
        let active = match self.state.get() {
            State::Measuring(active) => Some(active),
            State::Idle | State::Closed(_) => None,
            State::Armed(_) => return Err(Error::State.into()),
        };
        let timer = unsafe { &mut *self.timer.get() };
        match unsafe { timer.accept_interrupt(&interrupt) } {
            Ok(false) => return Ok(false),
            Ok(true) => {}
            Err(error) => execution
                .terminal
                .reject(format_args!("FAIL Fund timer {error:?}")),
        }
        if let Some(active) = active {
            if let Err(error) = execution.request_stop(interrupt, active.occupancy) {
                // Any accepted comparison can be the only remaining stop
                // coverage. Leave the original occupancy even when Sign
                // supplied the earlier trigger. Never resume it uncovered.
                execution
                    .terminal
                    .reject(format_args!("FAIL Fund stop {error:?}"));
            }
        }
        Ok(true)
    }

    pub(super) fn end(&self, current: &Current, value: u64) -> ! {
        let execution = current.execution;
        let (_, local) = execution.local().expect("funded local execution");
        let State::Measuring(mut active) = self.state.get() else {
            execution
                .terminal
                .reject(format_args!("FAIL Fund terminal occupation"));
        };
        if local.running() != Some(active.occupancy)
            || current.unit != active.occupancy.unit()
            || self.check_local().is_err()
            || !treaty::can_switch()
        {
            execution
                .terminal
                .reject(format_args!("FAIL Fund terminal gate"));
        }
        active.result = Some(value);
        self.state.set(State::Measuring(active));
        unsafe { local.leave(active.occupancy, active.context) };
        execution
            .terminal
            .reject(format_args!("FAIL Fund terminal continuation resumed"));
    }
}

impl Execution {
    pub fn attach_fund(&'static self, fund: &'static Fund<'static>) -> Result<(), FundError> {
        let (processor, _) = self.dispatcher()?;
        fund.check_local()?;
        if fund.execution.get().is_some() {
            return Err(Error::Busy.into());
        }
        self.room
            .acquire(|room| {
                let records = fund
                    .periods
                    .take()
                    .ok_or(crate::basis::room::Error::State)?;
                if let Err((error, records)) =
                    room.attach_fund(processor, fund.id(), fund.config.quota, records)
                {
                    fund.periods.set(Some(records));
                    return Err(error);
                }
                fund.execution.set(Some(ptr::from_ref(self).addr()));
                Ok::<_, crate::basis::room::Error>(())
            })
            .map_err(Error::from)?
            .map_err(Error::from)?;
        Ok(())
    }

    pub fn funded_period(&self, fund: &Fund<'_>) -> Result<PeriodKey, FundError> {
        let (processor, _) = self.dispatcher()?;
        fund.check_local()?;
        fund.check_execution(self)?;
        Ok(self
            .room
            .acquire(|room| room.funded_period(processor, fund.id()))
            .map_err(Error::from)?
            .map_err(Error::from)?)
    }

    /// Observe a previously closed, immutable original Period.
    pub fn closed_funded_period(
        &self,
        fund: &Fund<'_>,
        key: PeriodKey,
    ) -> Result<PeriodSettlement, FundError> {
        let (processor, _) = self.dispatcher()?;
        fund.check_local()?;
        fund.check_execution(self)?;
        Ok(self
            .room
            .acquire(|room| room.closed_period(processor, fund.id(), key))
            .map_err(Error::from)?
            .map_err(Error::from)?)
    }

    /// Close the exact old window after its boundary is reached and its
    /// execution and charge are settled. Open the next pre-admitted window,
    /// if any; the final close exhausts the finite schedule.
    /// Neither the original Sign registrations nor the Timer are changed.
    pub fn advance_funded_period(
        &self,
        fund: &Fund<'_>,
        expected: PeriodKey,
    ) -> Result<PeriodSettlement, FundError> {
        let (processor, _) = self.dispatcher()?;
        fund.check_local()?;
        fund.check_execution(self)?;
        if !matches!(fund.state.get(), State::Idle) {
            return Err(Error::Busy.into());
        }
        let (_, shuts) = fund.window(expected)?;
        Ok(self
            .room
            .acquire(|room| {
                if room.funded_period(processor, fund.id())? != expected {
                    return Err(crate::basis::room::Error::Gone);
                }
                let sample = fund
                    .timing
                    .sample()
                    .ok_or(crate::basis::room::Error::Timing)?;
                if !fund.timing.reached(sample, shuts) {
                    return Err(crate::basis::room::Error::Timing);
                }
                room.advance_period(processor, fund.id(), expected)
            })
            .map_err(Error::from)?
            .map_err(Error::from)?)
    }

    /// A funded Unit uses the exact Timer profile for every admission.
    ///
    /// # Safety
    /// In addition to the fixed-local kernel entry requirements, the entry
    /// must enable IRQs only within the approved resident integer span,
    /// with publication ordered before enabling delivery. It must close
    /// that span with IRQs masked. Every live value at an interrupted PC
    /// must permit suspension and later resumption on this same processor.
    /// While masked, the start and exit paths must fit the Fund's certified
    /// bounds. The span may not take ordinary locks or alter machine setup.
    pub unsafe fn create_funded(
        &self,
        fund: &'static Fund<'static>,
        entry: Entry,
        span: Range<usize>,
    ) -> Result<UnitId, Error> {
        fund.check_local()?;
        fund.check_execution(self)?;
        // The initial local Nest has not qualified the funded native
        // entry and stop profile with its new translation environment.
        if self.nest.is_some() {
            return Err(Error::Unsupported);
        }
        if span.is_empty() {
            return Err(Error::State);
        }
        let affinity = treaty::try_current_processor().ok_or(Error::Protection)?;
        self.publish_unit(KernelBinding {
            entry,
            affinity: Some(affinity),
            queue: Queue::Funded(fund.id()),
            nest: None,
            span: Some((span.start, span.end)),
        })
    }

    /// Recheck the bound Sign on the original comparator, without a live
    /// Charge. An expired setting is left for its actual native receiver.
    pub fn service_funded_deadlines<const N: usize>(
        &self,
        fund: &Fund<'_>,
        sign: &Sign<'_, N>,
        budget: NonZeroUsize,
    ) -> Result<DeadlineProgress, FundError> {
        self.dispatcher()?;
        fund.check_local()?;
        fund.check_execution(self)?;
        fund.check_sign(sign)?;
        if !matches!(fund.state.get(), State::Idle) {
            return Err(Error::Busy.into());
        }
        let (next, due) = self.settle_deadlines(sign, budget.get().min(N))?;
        let timer = unsafe { &mut *fund.timer.get() };
        let progress = Self::program_deadlines(sign, timer, next, due)?;
        if next.is_none() && !due && progress == DeadlineProgress::Armed {
            // There is no live Charge or Sign registration left to cover.
            // All publication of these local waits occurs on this processor.
            unsafe { timer.clear() }?;
            return Ok(DeadlineProgress::Idle);
        }
        Ok(progress)
    }

    /// Confirm coverage of the original stop bound before opening the Meter.
    /// The actual setting can be an earlier comparison owned by the same Timer.
    pub fn enter_funded<const N: usize>(
        &'static self,
        fund: &'static Fund<'static>,
        sign: &Sign<'_, N>,
    ) -> Result<Option<Occupancy>, FundError> {
        let (processor, local) = self.dispatcher()?;
        fund.check_local()?;
        fund.check_execution(self)?;
        if !matches!(fund.state.get(), State::Idle) {
            return Err(Error::Busy.into());
        }
        match self.service_funded_deadlines(fund, sign, NonZeroUsize::MIN)? {
            DeadlineProgress::Armed | DeadlineProgress::Idle => {}
            DeadlineProgress::Recheck | DeadlineProgress::AwaitInterrupt => {
                return Err(Error::Busy.into());
            }
        }
        let config = &fund.config;
        let Some((unit, period)) = self
            .room
            .acquire(|room| room.funded_candidate(processor, fund.id(), config.ceiling))
            .map_err(Error::from)?
            .map_err(Error::from)?
        else {
            return Ok(None);
        };
        let (opens, shuts) = fund.window(period)?;
        let estimate = fund.timing.sample().ok_or(Error::Timing)?;
        if !fund.timing.in_window(estimate, opens, shuts) {
            return Err(Error::Timing.into());
        }
        let planned = fund
            .timing
            .stop(estimate, shuts, config.ceiling, config.stop)
            .ok_or(Error::Timing)?;
        let prepared = (|| {
            let timer = unsafe { &mut *fund.timer.get() };
            let clock = fund.timing.clock();
            let target = clock.count_for(planned).ok_or(Error::Timing)?;
            let armed = timer
                .confirmed_deadline()?
                .map(|deadline| clock.count_for(deadline).ok_or(Error::Timing))
                .transpose()?;
            match armed {
                None => timer.arm_at(planned)?,
                Some(old) if old > target => timer.advance_deadline(planned)?,
                Some(_) => {}
            }
            let confirmed = timer.confirmed_deadline()?.ok_or(Error::Timing)?;
            Ok(self.with_unit_room(|units, room| {
                let (binding, context) = units.live(unit)?;
                let entry = binding.entry;
                let (begin, end) = binding.span.ok_or(Error::State)?;
                let opening = fund.timing.sample().ok_or(Error::Timing)?;
                if !fund
                    .timing
                    .can_start(opening, opens, planned, confirmed, config.stop.start)
                {
                    return Err(Error::Timing);
                }
                let occupancy =
                    room.occupy_funded(processor, fund.id(), unit, period, config.ceiling)?;
                // The prepaid Charge and preallocated local Meter become
                // Armed together. Dispatch can still be safely cancelled.
                fund.state.set(State::Armed(occupancy));
                Ok((occupancy, context, entry, begin..end))
            })?)
        })();
        let (occupancy, context, entry, span) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                // No Charge or Start committed. Keep any native effect and
                // existing Sign coverage for the next dispatcher recheck.
                return Err(error);
            }
        };
        let start = Start {
            execution: self,
            entry,
            fund: Some(fund),
        };
        // A queued Turn release can wait on a remote successor. Recheck after
        // all ordinary protection has ended so that latency cannot silently
        // consume the certified final entry margin.
        let opening = {
            let timer = unsafe { &*fund.timer.get() };
            let confirmed = timer.confirmed_deadline().ok().flatten();
            fund.timing.sample().filter(|sample| {
                confirmed.is_some_and(|target| {
                    fund.timing
                        .can_start(*sample, opens, planned, target, config.stop.start)
                })
            })
        };
        let Some(opening) = opening else {
            assert!(matches!(fund.state.get(), State::Armed(original) if original == occupancy));
            unsafe { local.cancel_unstarted(occupancy) };
            fund.state.set(State::Closed(Closed {
                receipt: MeterReceipt::unused(occupancy),
                result: None,
            }));
            // On an unsettled timer effect the exact unused receipts remain
            // pending and the dispatcher can retry vacate_funded.
            self.vacate_funded(fund, occupancy)?;
            return Err(Error::Timing.into());
        };
        assert!(matches!(fund.state.get(), State::Armed(original) if original == occupancy));
        fund.state.set(State::Measuring(Active {
            occupancy,
            context,
            meter: Meter::start(occupancy, opening),
            result: None,
        }));
        unsafe { local.enter_recoverable(occupancy, context, (&raw const start).cast(), span) };
        // This is the first ordinary read on the physically restored
        // dispatcher. Receipt and original clock stay paired through retry.
        let end = fund.timing.sample();
        let State::Measuring(active) = fund.state.get() else {
            unreachable!("original measuring occupancy");
        };
        fund.state.set(State::Closed(Closed {
            receipt: active.meter.close(&fund.timing, end),
            result: active.result,
        }));
        Ok(Some(occupancy))
    }

    pub fn vacate_funded(
        &self,
        fund: &Fund<'_>,
        occupancy: Occupancy,
    ) -> Result<ChargeSettlement, FundError> {
        fund.check_local()?;
        fund.check_execution(self)?;
        let (_, local) = self.dispatcher()?;
        let State::Closed(closed) = fund.state.get() else {
            return Err(Error::Gone.into());
        };
        let leave = local
            .stopped()
            .filter(|leave| leave.occupancy() == occupancy)
            .ok_or(Error::Gone)?;
        if closed.receipt.occupancy() != occupancy {
            return Err(Error::Gone.into());
        }
        if leave.unused() != closed.receipt.is_unused() {
            return Err(Error::State.into());
        }
        let destination = match (
            closed.result,
            leave.suspended() || leave.unused(),
            leave.discarded(),
        ) {
            (None, true, false) => Some(LeaveDestination::Ready),
            (Some(_), false, false) => Some(LeaveDestination::Release),
            // Cooperative Wait already published its exact destination with
            // Sign before physically leaving the Unit.
            (None, false, false) => None,
            _ => return Err(Error::State.into()),
        };
        fund.confirm_timer()?;
        let result = self.with_unit_room(|units, room| {
            units.live(occupancy.unit())?;
            let result = room.settle_funded(fund.id(), leave, closed.receipt, destination)?;
            if let Some(value) = closed.result {
                units
                    .begin_end(occupancy.unit(), End::Returned { value })
                    .expect("original live Unit");
            }
            local.settle(leave);
            fund.state.set(State::Idle);
            Ok(result)
        })?;
        Ok(result)
    }
}

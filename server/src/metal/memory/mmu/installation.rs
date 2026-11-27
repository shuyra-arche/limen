//! The actual local root register change shared by the x86 and Arm backends.

use super::SwitchError;

#[cfg(all(limen_target, any(target_arch = "aarch64", target_arch = "x86_64")))]
pub use bound::{LocalInstall, LocalRoot};

#[cfg(all(limen_target, any(target_arch = "aarch64", target_arch = "x86_64")))]
mod bound {
    use super::{Installation, SwitchError};
    #[cfg(target_arch = "aarch64")]
    use crate::metal::memory::mmu::aarch64::internal::{Controls, NativeRoot};
    #[cfg(target_arch = "x86_64")]
    use crate::metal::memory::mmu::x86_64::internal::{Controls, NativeRoot};
    use core::marker::PhantomData;
    use limen_treaty::model::PhysicalAddress;

    /// Hardware readback of the completed root installation on one Hart.
    /// Mapping, storage and residency retirement require their own evidence.
    #[derive(Debug, Eq, PartialEq)]
    pub struct LocalInstall {
        hart: usize,
        previous: PhysicalAddress,
        installed: PhysicalAddress,
    }

    impl LocalInstall {
        pub fn hart(&self) -> usize {
            self.hart
        }

        pub fn previous(&self) -> PhysicalAddress {
            self.previous
        }

        pub fn installed(&self) -> PhysicalAddress {
            self.installed
        }
    }

    /// Bound authority to switch the executing Hart's supported CPU root.
    /// Retains all affected native controls until the write is confirmed.
    #[must_use]
    pub struct LocalRoot {
        native: NativeRoot,
        installation: Installation<Controls>,
        local: PhantomData<*mut ()>,
    }

    impl LocalRoot {
        pub(in crate::metal::memory::mmu) fn bind(
            hart: usize,
            native: NativeRoot,
            initial: Controls,
        ) -> Self {
            Self {
                native,
                installation: Installation::new(hart, initial),
                local: PhantomData,
            }
        }

        /// Last confirmed root, absent while the original write is unsettled.
        pub fn current(&self) -> Option<PhysicalAddress> {
            self.installation
                .current()
                .map(|controls| self.native.root(controls))
        }

        /// Both roots retained during an uncertain local installation.
        pub fn pending(&self) -> Option<(PhysicalAddress, PhysicalAddress)> {
            self.installation.pending().map(|(previous, requested)| {
                (self.native.root(previous), self.native.root(requested))
            })
        }

        /// Install the requested root and complete the bound native maintenance.
        ///
        /// # Safety
        /// Both table trees must be live and coherent, and all code, stacks,
        /// handlers and data used across the transition must stay accessible.
        /// Retain the exact context and native admission obligations. Arm
        /// additionally requires a retained opposite root for the transition
        /// and excludes older accesses and conflicting ASID use on other PEs.
        pub unsafe fn switch(
            &mut self,
            expected: PhysicalAddress,
            requested: PhysicalAddress,
        ) -> Result<LocalInstall, SwitchError> {
            NativeRoot::check(self.installation.hart())?;
            let previous = self
                .installation
                .current()
                .ok_or(SwitchError::Unconfirmed)?;
            if self.native.root(previous) != expected {
                return Err(SwitchError::Current);
            }
            let next = self
                .native
                .with_root(previous, requested)
                .ok_or(SwitchError::Scope)?;
            let observed = unsafe { Controls::current() };
            self.installation.issue(previous, observed, next)?;
            unsafe { self.complete(next) }
        }

        /// Repeat only the retained installation on its original Hart.
        ///
        /// # Safety
        /// Retain the original switch's ownership, mappings and ordering.
        /// No unrelated operation may have used its controls.
        pub unsafe fn retry(&mut self) -> Result<LocalInstall, SwitchError> {
            NativeRoot::check(self.installation.hart())?;
            let observed = unsafe { Controls::current() };
            let next = self.installation.retry(observed)?;
            unsafe { self.complete(next) }
        }

        unsafe fn complete(&mut self, requested: Controls) -> Result<LocalInstall, SwitchError> {
            unsafe { self.native.install(requested) };
            let (previous, installed) =
                self.installation.complete(unsafe { Controls::current() })?;
            Ok(LocalInstall {
                hart: self.installation.hart(),
                previous: self.native.root(previous),
                installed: self.native.root(installed),
            })
        }
    }
}

enum State<C> {
    Current(C),
    Issued { previous: C, requested: C },
}

/// One local authority records the operation before its native write.
pub(super) struct Installation<C> {
    hart: usize,
    state: State<C>,
}

impl<C: Copy + Eq> Installation<C> {
    pub(super) fn new(hart: usize, current: C) -> Self {
        Self {
            hart,
            state: State::Current(current),
        }
    }

    pub(super) fn hart(&self) -> usize {
        self.hart
    }

    pub(super) fn current(&self) -> Option<C> {
        match self.state {
            State::Current(current) => Some(current),
            State::Issued { .. } => None,
        }
    }

    pub(super) fn pending(&self) -> Option<(C, C)> {
        match self.state {
            State::Current(_) => None,
            State::Issued {
                previous,
                requested,
            } => Some((previous, requested)),
        }
    }

    pub(super) fn issue(
        &mut self,
        expected: C,
        observed: C,
        requested: C,
    ) -> Result<(), SwitchError> {
        let previous = self.current().ok_or(SwitchError::Unconfirmed)?;
        if previous != expected || previous != observed {
            return Err(SwitchError::Current);
        }
        self.state = State::Issued {
            previous,
            requested,
        };
        Ok(())
    }

    pub(super) fn retry(&self, observed: C) -> Result<C, SwitchError> {
        let (previous, requested) = self.pending().ok_or(SwitchError::Current)?;
        if observed == previous || observed == requested {
            Ok(requested)
        } else {
            Err(SwitchError::Unconfirmed)
        }
    }

    /// Called after the original native write and maintenance have finished.
    pub(super) fn complete(&mut self, observed: C) -> Result<(C, C), SwitchError> {
        let (previous, requested) = self.pending().ok_or(SwitchError::Current)?;
        if observed != requested {
            return Err(SwitchError::Unconfirmed);
        }
        self.state = State::Current(requested);
        Ok((previous, requested))
    }
}

#[cfg(test)]
#[path = "../../../../tests/metal/memory/mmu/installation.rs"]
mod tests;

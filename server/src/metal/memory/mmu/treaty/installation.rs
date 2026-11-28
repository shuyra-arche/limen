//! Confirmed installation of the original local CPU translation context.

use super::SwitchError;

#[cfg(all(
    limen_target,
    any(
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "x86_64"
    )
))]
pub use bound::{Context, LocalContext, LocalInstall};

#[cfg(all(
    limen_target,
    any(
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "x86_64"
    )
))]
mod bound {
    use super::{Installation, SwitchError};
    #[cfg(target_arch = "aarch64")]
    use crate::metal::memory::mmu::aarch64::internal::{
        context::Controls, format::Format as Profile, installation::NativeRoot,
    };
    #[cfg(target_arch = "riscv64")]
    use crate::metal::memory::mmu::riscv64::internal::{
        context::Context as Controls, description::MaximumMode as Profile,
        installation::NativeRoot as Native,
    };
    #[cfg(target_arch = "x86_64")]
    use crate::metal::memory::mmu::x86_64::internal::{
        context::Controls, format::Format as Profile, installation::NativeRoot,
    };
    use core::marker::PhantomData;
    use limen_treaty::model::PhysicalAddress;

    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    type Native<'scope> = NativeRoot;

    /// Copyable description of the native state of one admitted local scope.
    /// Installation requires the original authority, table trees and continuity.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Context {
        processor: usize,
        profile: Profile,
        controls: Controls,
        root: Option<PhysicalAddress>,
    }

    impl Context {
        pub fn root(self) -> Option<PhysicalAddress> {
            self.root
        }
    }

    /// Hardware readback of this local installation and its original context.
    /// Storage, mapping and residency retirement require separate evidence.
    #[derive(Debug, Eq, PartialEq)]
    pub struct LocalInstall {
        processor: usize,
        previous: Context,
        installed: Context,
    }

    impl LocalInstall {
        pub fn processor(&self) -> usize {
            self.processor
        }

        pub fn previous(&self) -> Context {
            self.previous
        }

        pub fn installed(&self) -> Context {
            self.installed
        }
    }

    /// Bound access to the original processor's supported translation controls.
    /// Only its native admission may establish this local installation scope.
    #[must_use]
    pub struct LocalContext<'scope> {
        native: Native<'scope>,
        installation: Installation<Controls>,
        local: PhantomData<(&'scope (), *mut ())>,
    }

    impl<'scope> LocalContext<'scope> {
        pub(in crate::metal::memory::mmu) fn bind(
            processor: usize,
            native: Native<'scope>,
            initial: Controls,
        ) -> Self {
            Self {
                native,
                installation: Installation::new(processor, initial),
                local: PhantomData,
            }
        }

        fn describe(&self, controls: Controls) -> Context {
            Context {
                processor: self.installation.processor(),
                profile: self.native.profile(),
                controls,
                root: self.native.physical_root(controls),
            }
        }

        /// Last confirmed context, absent while the original write is unsettled.
        pub fn current(&self) -> Option<Context> {
            self.installation
                .current()
                .map(|value| self.describe(value))
        }

        /// Both exact contexts retained during an uncertain installation.
        pub fn pending(&self) -> Option<(Context, Context)> {
            self.installation
                .pending()
                .map(|(previous, requested)| (self.describe(previous), self.describe(requested)))
        }

        /// Install the prepared context and complete its local native maintenance.
        ///
        /// # Safety
        /// The original owners keep both actual table trees and all code,
        /// stacks, handlers and data used across the transition live and
        /// coherent. Retain the native admission obligations for both contexts
        /// through any unconfirmed effect, including other roots or tag users.
        /// Instruction visibility and remote synchronization remain with their
        /// respective owners.
        pub unsafe fn install(
            &mut self,
            expected: Context,
            requested: Context,
        ) -> Result<LocalInstall, SwitchError> {
            Native::check(self.installation.processor())?;
            let previous = self
                .installation
                .current()
                .ok_or(SwitchError::Unconfirmed)?;
            if self.describe(previous) != expected {
                return Err(SwitchError::Current);
            }
            if self.describe(requested.controls) != requested
                || !self.native.supports(previous, requested.controls)
            {
                return Err(SwitchError::Scope);
            }
            let observed = unsafe { Native::observe() };
            self.installation
                .issue(previous, observed, requested.controls)?;
            unsafe { self.apply(requested.controls) }
        }

        /// Confirm the installed controls of the original pending operation.
        /// Observation does not issue another native write or prove retirement.
        pub fn confirm(&mut self) -> Result<LocalInstall, SwitchError> {
            Native::check(self.installation.processor())?;
            self.installation.pending().ok_or(SwitchError::Current)?;
            self.settle(unsafe { Native::observe() })
        }

        /// Retry only the exact retained installation on its original processor.
        /// If its target is already observed, settle the original operation.
        ///
        /// # Safety
        /// Keep the original switch's ownership, mappings and ordering live.
        /// No unrelated operation may have used its controls.
        pub unsafe fn retry(&mut self) -> Result<LocalInstall, SwitchError> {
            Native::check(self.installation.processor())?;
            let observed = unsafe { Native::observe() };
            if let Some(next) = self.installation.retry(observed)? {
                unsafe { self.apply(next) }
            } else {
                self.settle(observed)
            }
        }

        unsafe fn apply(&mut self, requested: Controls) -> Result<LocalInstall, SwitchError> {
            unsafe { self.native.install(requested) };
            self.settle(unsafe { Native::observe() })
        }

        fn settle(&mut self, observed: Controls) -> Result<LocalInstall, SwitchError> {
            let (previous, installed) = self.installation.complete(observed)?;
            Ok(LocalInstall {
                processor: self.installation.processor(),
                previous: self.describe(previous),
                installed: self.describe(installed),
            })
        }
    }

    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    impl LocalContext<'_> {
        /// Qualify a retained root for the original native controls and format.
        ///
        /// # Safety
        /// The original owner supplies this exact live root and has checked
        /// access continuity, table visibility and the native scope conditions.
        pub unsafe fn prepare_root(&self, root: PhysicalAddress) -> Result<Context, SwitchError> {
            let previous = self
                .installation
                .current()
                .ok_or(SwitchError::Unconfirmed)?;
            let next = self
                .native
                .with_root(previous, root)
                .ok_or(SwitchError::Scope)?;
            Ok(self.describe(next))
        }
    }

    #[cfg(target_arch = "aarch64")]
    impl LocalContext<'_> {
        /// Qualify the owned upper tree against the original inactive walk.
        pub(in crate::metal::memory::mmu) unsafe fn prepare_table(
            &self,
            table: crate::metal::memory::mmu::table::TableFormat,
            root: PhysicalAddress,
        ) -> Result<Context, SwitchError> {
            use crate::metal::memory::mmu::{aarch64::internal::format::Root, table::TableFormat};
            let previous = self.current().ok_or(SwitchError::Unconfirmed)?;
            let format = self.native.profile();
            if previous.root().is_some()
                || format.root() != Root::Upper
                || table != TableFormat::Aarch64(format)
            {
                return Err(SwitchError::Scope);
            }
            unsafe { self.prepare_root(root) }
        }
    }

    #[cfg(target_arch = "x86_64")]
    impl LocalContext<'_> {
        /// Qualify an owned successor in the exact original IA-32e format.
        pub(in crate::metal::memory::mmu) unsafe fn prepare_table(
            &self,
            table: crate::metal::memory::mmu::table::TableFormat,
            root: PhysicalAddress,
        ) -> Result<Context, SwitchError> {
            use crate::metal::memory::mmu::table::TableFormat;
            let previous = self.current().ok_or(SwitchError::Unconfirmed)?;
            if table != TableFormat::X86_64(self.native.profile())
                || previous.root() == Some(root)
            {
                return Err(SwitchError::Scope);
            }
            unsafe { self.prepare_root(root) }
        }

        /// Observe one original PML4 entry through its retained identity view.
        /// The caller holds the real original subtree and its stable structure.
        pub(in crate::metal::memory::mmu) unsafe fn original_branch(
            &self,
            original: core::ptr::NonNull<u64>,
            index: usize,
        ) -> Result<u64, SwitchError> {
            Native::check(self.installation.processor())?;
            let previous = self.installation.current().ok_or(SwitchError::Unconfirmed)?;
            let observed = unsafe { Native::observe() };
            if previous != observed {
                return Err(SwitchError::Current);
            }
            if index >= 512 || original.as_ptr().addr() != previous.cr3 || previous.cr3 == 0 {
                return Err(SwitchError::Scope);
            }
            // A volatile scalar observation, not an exclusive byte reference:
            // hardware may update Accessed on the retained original root.
            let entry = unsafe { original.as_ptr().add(index).read_volatile() };
            const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
            if entry & ADDRESS == previous.cr3 as u64 {
                // Recursive roots are not independent retained subtrees.
                return Err(SwitchError::Scope);
            }
            self.native.profile().inherited_branch(entry).ok_or(SwitchError::Scope)
        }
    }

    #[cfg(target_arch = "riscv64")]
    impl LocalContext<'_> {
        /// Qualify the owned successor of the original bare supervisor context.
        pub(in crate::metal::memory::mmu) unsafe fn prepare_table(
            &self,
            table: crate::metal::memory::mmu::table::TableFormat,
            root: PhysicalAddress,
        ) -> Result<Context, SwitchError> {
            use crate::metal::memory::mmu::table::TableFormat;
            let previous = self.current().ok_or(SwitchError::Unconfirmed)?;
            let TableFormat::Riscv64(format) = table else {
                return Err(SwitchError::Scope);
            };
            if previous.controls != Controls::BARE {
                return Err(SwitchError::Scope);
            }
            let next = Controls::paged(format, root).ok_or(SwitchError::Scope)?;
            unsafe { self.prepare_context(next) }
        }

        /// Qualify the exact retained Bare or ASID-zero paged context.
        ///
        /// # Safety
        /// The original owner supplies this exact context and has checked
        /// access continuity, table visibility and the native scope conditions.
        pub unsafe fn prepare_context(&self, next: Controls) -> Result<Context, SwitchError> {
            let previous = self
                .installation
                .current()
                .ok_or(SwitchError::Unconfirmed)?;
            if !self.native.supports(previous, next) {
                return Err(SwitchError::Scope);
            }
            Ok(self.describe(next))
        }
    }
}

enum State<C> {
    Current(C),
    Issued { previous: C, requested: C },
}

/// Tracks a local installation and the observations needed to confirm it.
/// Constructing the record grants no access to native controls.
pub struct Installation<C> {
    processor: usize,
    state: State<C>,
}

impl<C: Copy + Eq> Installation<C> {
    pub fn new(processor: usize, current: C) -> Self {
        Self {
            processor,
            state: State::Current(current),
        }
    }

    pub fn processor(&self) -> usize {
        self.processor
    }

    pub fn current(&self) -> Option<C> {
        match self.state {
            State::Current(current) => Some(current),
            State::Issued { .. } => None,
        }
    }

    pub fn pending(&self) -> Option<(C, C)> {
        match self.state {
            State::Current(_) => None,
            State::Issued {
                previous,
                requested,
            } => Some((previous, requested)),
        }
    }

    pub fn issue(&mut self, expected: C, observed: C, requested: C) -> Result<(), SwitchError> {
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

    /// Return the original requested controls only if they still need reapplying.
    /// An exact observation of the requested controls can settle without a write.
    pub fn retry(&self, observed: C) -> Result<Option<C>, SwitchError> {
        let (previous, requested) = self.pending().ok_or(SwitchError::Current)?;
        if observed == requested {
            Ok(None)
        } else if observed == previous {
            Ok(Some(requested))
        } else {
            Err(SwitchError::Unconfirmed)
        }
    }

    /// Called after the original native write and maintenance have finished.
    pub fn complete(&mut self, observed: C) -> Result<(C, C), SwitchError> {
        let (previous, requested) = self.pending().ok_or(SwitchError::Current)?;
        if observed != requested {
            return Err(SwitchError::Unconfirmed);
        }
        self.state = State::Current(requested);
        Ok((previous, requested))
    }
}

#[cfg(test)]
#[path = "../../../../../tests/metal/memory/mmu/installation.rs"]
mod tests;

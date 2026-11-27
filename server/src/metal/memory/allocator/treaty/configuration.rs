use crate::metal::{identity::treaty::Entry, sync::treaty::TurnLock};
use core::alloc::Layout;

use super::super::{
    allocation::AllocationRecord,
    allowance::AllowanceSlot,
    bootstrap::array,
    directory::Link,
    region::{Region, buddy::Root},
    returns::ReturnRecord,
};
use super::{Allocator, Config, Error};

impl Config {
    /// Full fixed root, including backing for delayed admission and history.
    pub fn layout(self) -> Result<Layout, Error> {
        self.check()?;
        let mut root = Layout::new::<Allocator>();
        for element in [
            array::<Region>(self.regions)?,
            array::<Link>(self.regions)?,
            array::<TurnLock<AllowanceSlot>>(self.allowances)?,
        ] {
            root = root.extend(element).map_err(|_| Error::Layout)?.0;
        }
        let region = [
            array::<Root>(self.roots)?,
            array::<u8>(self.nodes()?)?,
            array::<Entry<AllocationRecord>>(self.allocations)?,
            array::<Entry<ReturnRecord>>(self.returns)?,
        ];
        for _ in 0..self.regions {
            for element in region {
                root = root.extend(element).map_err(|_| Error::Layout)?.0;
            }
        }
        Ok(root.pad_to_align())
    }
}

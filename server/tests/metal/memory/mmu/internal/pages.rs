use limen_treaty::model::PhysicalAddress;

// The board fixture grants its image identity mappings and exclusive
// ownership of these static pages before any secondary is started.
#[repr(C, align(4096))]
pub(super) struct Page(pub(super) [u64; 512]);
pub(super) static mut TOP: Page = Page([0; 512]);
pub(super) static mut NEXT: Page = Page([0; 512]);
pub(super) static mut MIDDLE: Page = Page([0; 512]);
pub(super) static mut LAST: Page = Page([0; 512]);
pub(super) static DATA: Page = Page([0xabcd_dcba_1234_5678; 512]);

pub(super) fn physical(page: *const Page) -> PhysicalAddress {
    PhysicalAddress::new(page.addr() as u64)
}

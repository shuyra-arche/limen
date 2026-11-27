#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub(super) use x86_64::check;
#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub(super) use aarch64::check;

use limen_treaty::model::PhysicalAddress;

// The board fixture grants its image identity mappings and exclusive
// ownership of these static pages before any secondary is started.
#[repr(C, align(4096))]
struct Page([u64; 512]);
static mut TOP: Page = Page([0; 512]);
static mut NEXT: Page = Page([0; 512]);
static mut MIDDLE: Page = Page([0; 512]);
static mut LAST: Page = Page([0; 512]);
static DATA: Page = Page([0xabcd_dcba_1234_5678; 512]);

fn physical(page: *const Page) -> PhysicalAddress {
    PhysicalAddress::new(page.addr() as u64)
}

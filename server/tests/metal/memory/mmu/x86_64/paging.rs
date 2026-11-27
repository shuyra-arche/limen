#[path = "../internal/pages.rs"]
mod pages;
use core::{arch::asm, ptr};
use kernel::metal::memory::access::treaty::device::DeviceMapping;
use kernel::metal::memory::mmu::{
    treaty::{LeafAccess, Memory, Privilege, SwitchError},
    x86_64::internal::{context::Controls, installation::admit_root},
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress, PhysicalRange};
use pages::{DATA, LAST, MIDDLE, NEXT, Page, TOP, physical};

#[path = "../internal/installation.rs"]
mod installation;

static mut ALTERNATE_TOP: Page = Page([0; 512]);
static mut ALTERNATE_NEXT: Page = Page([0; 512]);
static mut ALTERNATE_MIDDLE: Page = Page([0; 512]);
static mut ALTERNATE_LAST: Page = Page([0; 512]);
static ALTERNATE_DATA: Page = Page([0x7654_3210_abcd_dcba; 512]);

fn roundtrip(hart: usize, apic: PhysicalRange, version: u32) {
    assert!(matches!(
        unsafe { admit_root(hart.wrapping_add(1)) },
        Err(SwitchError::Context)
    ));
    let (mut control, format) = unsafe { admit_root(hart) }.expect("q35 four-level CPU paging");
    let original = unsafe { Controls::current() };
    assert_eq!(original.cr3 & 0xfff, 0);
    // The fixture retains the identity-mapped firmware table pages.
    let old = ptr::with_exposed_provenance::<u64>(original.cr3);
    let top = unsafe { &raw mut TOP.0 };
    let root = physical(&raw const TOP);
    let alternate = physical(&raw const ALTERNATE_TOP);
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") 0x277u32, out("eax") low, out("edx") high, options(nomem, nostack));
        let pat = u64::from(high) << 32 | u64::from(low);
        for i in 0..512 {
            (*top)[i] = old.add(i).read_volatile();
            ALTERNATE_TOP.0[i] = (*top)[i];
        }
        let slot = (1..256)
            .find(|&i| (*top)[i] == 0)
            .expect("free lower PML4 entry");
        (*top)[slot] = format
            .branch(physical(&raw const NEXT), Privilege::Kernel)
            .unwrap();
        NEXT.0[0] = format
            .branch(physical(&raw const MIDDLE), Privilege::Kernel)
            .unwrap();
        MIDDLE.0[0] = format
            .branch(physical(&raw const LAST), Privilege::Kernel)
            .unwrap();
        LAST.0[0] = format
            .leaf(
                physical(&raw const DATA),
                0,
                Privilege::Kernel,
                LeafAccess::ReadOnly,
                Memory::Normal,
                pat,
            )
            .unwrap();
        LAST.0[1] = format
            .leaf(
                apic.start(),
                0,
                Privilege::Kernel,
                LeafAccess::ReadOnly,
                Memory::Device,
                pat,
            )
            .unwrap();
        ALTERNATE_TOP.0[slot] = format
            .branch(physical(&raw const ALTERNATE_NEXT), Privilege::Kernel)
            .unwrap();
        ALTERNATE_NEXT.0[0] = format
            .branch(physical(&raw const ALTERNATE_MIDDLE), Privilege::Kernel)
            .unwrap();
        ALTERNATE_MIDDLE.0[0] = format
            .branch(physical(&raw const ALTERNATE_LAST), Privilege::Kernel)
            .unwrap();
        ALTERNATE_LAST.0[0] = format
            .leaf(
                physical(&raw const ALTERNATE_DATA),
                0,
                Privilege::Kernel,
                LeafAccess::ReadOnly,
                Memory::Normal,
                pat,
            )
            .unwrap();
        let first = control.prepare_root(root).unwrap();
        let alternate_context = control.prepare_root(alternate).unwrap();
        assert_eq!(first.root(), Some(root));
        assert!(matches!(
            control.prepare_root(PhysicalAddress::new(root.get() + 1)),
            Err(SwitchError::Scope)
        ));
        installation::roundtrip(&mut control, hart, first, |control| {
            let data: u64;
            asm!("mov {}, [{}]", out(reg) data, in(reg) slot << 39, options(nostack, readonly));
            assert_eq!(data, DATA.0[0]);
            // Hardware set Accessed on this newly installed leaf.
            assert_ne!((&raw const LAST.0[0]).read_volatile() & (1 << 5), 0);

            // The read-only APIC alias exists only in this installed root.
            // Keep its exclusive root control borrowed until every child view ends.
            let alias = InternalAddress::new(((slot as u64) << 39) + 4096);
            let mut mapping = DeviceMapping::admit(apic, alias, &mut *control)
                .ok()
                .unwrap();
            {
                let view = mapping.window();
                let (mut registers, other) = view.split(ByteLength::new(0x100)).ok().unwrap();
                assert_eq!(registers.read_u32(0x30), Ok(version));
                assert_eq!(registers.physical().end(), other.physical().start());
            }
            assert_eq!(mapping.into_retention().current(), Some(first));

            // The same virtual address must use the second root's translation
            // after the native CR3 switch flushes the first.
            let second = installation::install(control, first, alternate_context);
            assert_eq!(second.installed().root(), Some(alternate));
            let data: u64;
            asm!("mov {}, [{}]", out(reg) data, in(reg) slot << 39, options(nostack, readonly));
            assert_eq!(data, ALTERNATE_DATA.0[0]);
            assert_ne!(
                (&raw const ALTERNATE_LAST.0[0]).read_volatile() & (1 << 5),
                0
            );
        });
    }
    assert_eq!(unsafe { Controls::current() }, original);
    kernel::metal::platform::boot::treaty::write(
        "PASS internal paging switched=1 alias=1 restored=1\n",
    );
    kernel::metal::platform::boot::treaty::write(
        "PASS local-context original=1 installed=1 restored=1\n",
    );
    kernel::metal::platform::boot::treaty::write(
        "PASS device mapping fixed=1 dynamic=1 split=1 restored=1\n",
    );
}

pub fn check(boot: kernel::entry::boot::Boot<'_>) {
    uefi_fixture::check_handoff(&boot, None);
    let (apic, version) = {
        let mut fixed = uefi_fixture::q35::local_window(&boot);
        (fixed.physical(), unsafe { fixed.read_u32(0x30) }.unwrap())
    };
    roundtrip(boot.hart(), apic, version);
}

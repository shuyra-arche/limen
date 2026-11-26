//! Firmware calls required by the trusted boot prefix.

use super::super::facts::{Descriptions, Facts};
use crate::metal::{boot::services::Services, hart::riscv::description::RiscvCpus};
use core::arch::asm;
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

use super::probe::{DEBUG_CONSOLE, Extensions, HART_STATE_MANAGEMENT, SYSTEM_RESET, SbiServices};

const BASE: usize = 0x10;
const HART_STATUS: usize = 2;
const STARTED: usize = 0;

fn extensions() -> Extensions {
    Extensions::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

/// Probe the running SBI implementation for its HSM, timer and IPI services.
/// The current CPU must be executing in the supported SBI supervisor environment.
pub fn platform_services() -> SbiServices {
    SbiServices::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

fn write_byte(byte: u8) -> bool {
    call(DEBUG_CONSOLE, 2, usize::from(byte), 0, 0).0 == 0
}

/// End the trusted boot path through SBI system reset.
fn reset(success: bool) {
    // Shutdown with no reason (0) or system failure (1).
    let _ = call(SYSTEM_RESET, 0, 0, usize::from(!success), 0);
}

pub(crate) fn start_hart(hart: usize, address: usize, opaque: usize) -> Result<(), isize> {
    match call(HART_STATE_MANAGEMENT, 0, hart, address, opaque).0 {
        0 => Ok(()),
        error => Err(error),
    }
}

fn call(
    extension: usize,
    function: usize,
    first: usize,
    second: usize,
    third: usize,
) -> (isize, usize) {
    let (error, value);
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") first => error,
            inlateout("a1") second => value,
            in("a2") third,
            in("a6") function,
            in("a7") extension,
            options(nostack),
        );
    }
    (error, value)
}

static SERVICES: Services = Services {
    output: Some(write_byte),
    reset: Some(reset),
};

/// The trap vector is live before BSS has been cleared. Its failure path
/// probes firmware directly and never depends on a published service table.
pub(crate) fn trap_services() -> Services {
    let available = extensions();
    Services {
        output: SERVICES.output.filter(|_| available.console),
        reset: SERVICES.reset.filter(|_| available.reset),
    }
}

pub(crate) fn services_on_entry() -> &'static Services {
    let available = trap_services();
    if available.output.is_none() || available.reset.is_none() {
        available.reject(format_args!(
            "FAIL Limen boot: required SBI debug console or system reset unavailable\n"
        ));
    }
    SERVICES.install();
    &SERVICES
}

pub(crate) unsafe fn admit<'a>(
    hart: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    // The image provides the readable physical window independently of the
    // a1 pointer or the bytes of the DTB. These symbols are addresses, not objects.
    unsafe extern "C" {
        static DTB_WINDOW_START: u8;
        static DTB_WINDOW_END: u8;
    }
    let window = (&raw const DTB_WINDOW_START).addr()..(&raw const DTB_WINDOW_END).addr();
    let image = crate::metal::boot::image();
    let parsed = unsafe {
        super::super::devicetree::memory::read(
            input,
            window,
            image.clone(),
            &mut descriptions.memory,
        )
    };
    let tree = match parsed {
        Ok(tree) => tree,
        Err(error) => Services::active().reject(format_args!("FAIL Limen boot: DTB {error:?}\n")),
    };
    let cpus = RiscvCpus::new(tree).and_then(|cpus| {
        cpus.as_ref().map_or(Ok(false), |cpus| {
            cpus.cpu(hart as u64)
                .map(|cpu| cpu.is_some_and(|cpu| cpu.enabled()))
        })
    });
    match cpus {
        Ok(true) => {}
        Ok(false) => Services::active().reject(format_args!(
            "FAIL Limen boot: firmware Hart missing or disabled in DTB\n"
        )),
        Err(error) => {
            Services::active().reject(format_args!("FAIL Limen boot: DTB CPU {error:?}\n"))
        }
    }
    // The entry ABI supplies the current Hart identity in a0. HSM can also
    // report the state of that named Hart when the extension is available.
    if platform_services().hart_state_management()
        && call(HART_STATE_MANAGEMENT, HART_STATUS, hart, 0, 0) != (0, STARTED)
    {
        Services::active().reject(format_args!("FAIL Limen boot: SBI Hart status mismatch\n"));
    }
    descriptions.device_tree = Some(tree);
    let image = PhysicalRange::new(
        PhysicalAddress::new(image.start as u64),
        PhysicalAddress::new(image.end as u64),
    )?;
    Some(Facts {
        image,
        firmware_map: None,
    })
}

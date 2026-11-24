pub(super) const DEBUG_CONSOLE: usize = 0x4442_434e;
pub(super) const SYSTEM_RESET: usize = 0x5352_5354;

pub(crate) struct Extensions {
    pub(crate) console: bool,
    pub(crate) reset: bool,
}

impl Extensions {
    pub(crate) fn probe(mut base: impl FnMut(usize, usize) -> (isize, usize)) -> Self {
        let (error, version) = base(0, 0);
        let modern = error == 0 && version >= 2;
        let mut supports = |extension| {
            let (error, value) = base(3, extension);
            error == 0 && value != 0
        };
        Self {
            console: modern && supports(DEBUG_CONSOLE),
            reset: modern && supports(SYSTEM_RESET),
        }
    }
}

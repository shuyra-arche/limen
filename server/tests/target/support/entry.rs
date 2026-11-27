//! The standard guest image boundary. Test behavior stays in the caller.

#[macro_export]
macro_rules! entry {
    (
        platform: $platform:expr,
        $(prepare: $prepare:expr,)?
        $(major: ($stack:expr, $nodes:expr),)?
        run: $($run:tt)+
    ) => {
        $crate::declare!($platform);

        #[cfg(target_arch = "riscv64")]
        $crate::entry!(@major $($stack, $nodes)?);

        #[cfg(target_os = "uefi")]
        #[uefi::entry]
        fn main() -> uefi::Status {
            $crate::entry!(@load $($prepare)?)
        }

        $crate::entry!(@ready $($run)+);

        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
            $crate::reject(info)
        }
    };
    (@major) => {
        $crate::major!(stack: 32 * 1024, nodes: 5);
    };
    (@major $stack:expr, $nodes:expr) => {
        $crate::major!(stack: $stack, nodes: $nodes);
    };
    (@load) => {
        $crate::load(|| Ok(()))
    };
    (@load $prepare:expr) => {
        $crate::load($prepare)
    };
    (@ready |$boot:ident| $body:block) => {
        #[unsafe(no_mangle)]
        extern "Rust" fn limen_platform_ready($boot: $crate::Boot<'_>) -> ! $body
    };
    (@ready |mut $boot:ident| $body:block) => {
        #[unsafe(no_mangle)]
        extern "Rust" fn limen_platform_ready(mut $boot: $crate::Boot<'_>) -> ! $body
    };
}

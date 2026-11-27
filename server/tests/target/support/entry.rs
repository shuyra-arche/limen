//! The standard guest image boundary. Behavior determines the harness.
#[macro_export]
macro_rules! entry {
    (
        platform: $platform:expr,
        $(prepare: $prepare:expr,)?
        $(major: ($stack:expr, $nodes:expr),)?
        test: collected $(=> $after:expr)?,
        $($(#[cfg($($condition:tt)*)])? bench: $bench:expr,)?
    ) => {
        $crate::entry! {
            @select [
                platform: $platform,
                $(prepare: $prepare,)?
                $(major: ($stack, $nodes),)?
            ]
            $crate::run, $crate::entry!(@callback $($after)?),
            $($(#[cfg($($condition)*)])? bench: $bench,)?
        }
    };
    (
        platform: $platform:expr,
        $(prepare: $prepare:expr,)?
        $(major: ($stack:expr, $nodes:expr),)?
        test: $test:expr,
        $($(#[cfg($($condition:tt)*)])? bench: $bench:expr,)?
    ) => {
        $crate::entry! {
            @select [
                platform: $platform,
                $(prepare: $prepare,)?
                $(major: ($stack, $nodes),)?
            ]
            $crate::scenario, $test,
            $($(#[cfg($($condition)*)])? bench: $bench,)?
        }
    };
    (
        platform: $platform:expr,
        $(prepare: $prepare:expr,)?
        $(major: ($stack:expr, $nodes:expr),)?
        bench: $bench:expr,
    ) => {
        $crate::entry! {
            @emit [
                platform: $platform,
                $(prepare: $prepare,)?
                $(major: ($stack, $nodes),)?
            ]
            $crate::Purpose::Bench, $crate::scenario, $bench
        }
    };
    (@callback) => { |_: $crate::Boot<'_>| {} };
    (@callback $callback:expr) => { $callback };
    (@select [$($setup:tt)*] $harness:path, $test:expr,) => {
        $crate::entry!(@emit [$($setup)*] $crate::Purpose::Test, $harness, $test);
    };
    (
        @select [$($setup:tt)*] $harness:path, $test:expr,
        $(#[cfg($($condition:tt)*)])? bench: $bench:expr,
    ) => {
        #[cfg(not(all(feature = "bench" $(, $($condition)*)?)))]
        $crate::entry!(@emit [$($setup)*] $crate::Purpose::Test, $harness, $test);
        #[cfg(all(feature = "bench" $(, $($condition)*)?))]
        $crate::entry!(@emit [$($setup)*] $crate::Purpose::Bench, $crate::scenario, $bench);
    };
    (
        @emit [
            platform: $platform:expr,
            $(prepare: $prepare:expr,)?
            $(major: ($stack:expr, $nodes:expr),)?
        ]
        $purpose:expr, $harness:path, $operation:expr
    ) => {
        $crate::declare!($platform, $purpose);

        #[cfg(target_arch = "riscv64")]
        $crate::entry!(@major $($stack, $nodes)?);

        #[cfg(target_os = "uefi")]
        #[uefi::entry]
        fn main() -> uefi::Status {
            $crate::entry!(@load $($prepare)?)
        }

        fn harness(cases: &[&dyn Fn()]) {
            $harness(cases);
        }

        #[unsafe(no_mangle)]
        extern "Rust" fn limen_platform_ready(boot: $crate::Boot<'_>) -> ! {
            collected();
            let operation: fn($crate::Boot<'_>) = $operation;
            operation(boot);
            $crate::finish()
        }

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
}

//! The standard guest image boundary. Behavior determines the harness.
#[macro_export]
macro_rules! entry {
    (
        platform: $platform:expr,
        $(prepare: $prepare:expr,)?
        $(major: ($stack:expr, $nodes:expr),)?
        test: { $($test:tt)* },
        $(#[cfg($($condition:tt)*)])?
        bench: { $bench:expr; $($bench_item:item)* },
    ) => {
        #[cfg(not(all(feature = "bench" $(, $($condition)*)?)))]
        $crate::entry! {
            @test [
                platform: $platform,
                $(prepare: $prepare,)?
                $(major: ($stack, $nodes),)?
            ]
            $($test)*
        }
        #[cfg(all(feature = "bench" $(, $($condition)*)?))]
        $crate::entry! {
            @emit [
                platform: $platform,
                $(prepare: $prepare,)?
                $(major: ($stack, $nodes),)?
            ]
            [$($bench_item)*]
            $crate::Purpose::Bench, $crate::scenario, $bench
        }
    };
    (
        platform: $platform:expr,
        $(prepare: $prepare:expr,)?
        $(major: ($stack:expr, $nodes:expr),)?
        test: collected $(=> $after:expr)?,
    ) => {
        $crate::entry! {
            @test [
                platform: $platform,
                $(prepare: $prepare,)?
                $(major: ($stack, $nodes),)?
            ]
            collected $(=> $after)?;
        }
    };
    (
        platform: $platform:expr,
        $(prepare: $prepare:expr,)?
        $(major: ($stack:expr, $nodes:expr),)?
        test: $test:expr,
    ) => {
        $crate::entry! {
            @test [
                platform: $platform,
                $(prepare: $prepare,)?
                $(major: ($stack, $nodes),)?
            ]
            $test;
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
            []
            $crate::Purpose::Bench, $crate::scenario, $bench
        }
    };
    (
        @test [$($setup:tt)*]
        collected $(=> $after:expr)?; $($item:item)*
    ) => {
        $crate::entry! {
            @emit [$($setup)*] [$($item)*]
            $crate::Purpose::Test, $crate::run, $crate::entry!(@callback $($after)?)
        }
    };
    (
        @test [$($setup:tt)*]
        $test:expr; $($item:item)*
    ) => {
        $crate::entry! {
            @emit [$($setup)*] [$($item)*]
            $crate::Purpose::Test, $crate::scenario, $test
        }
    };
    (@callback) => { |_: $crate::Boot<'_>| {} };
    (@callback $callback:expr) => { $callback };
    (
        @emit [
            platform: $platform:expr,
            $(prepare: $prepare:expr,)?
            $(major: ($stack:expr, $nodes:expr),)?
        ]
        [$($item:item)*]
        $purpose:expr, $harness:path, $operation:expr
    ) => {
        // Only the selected behavior contributes its Rust items and cases.
        $($item)*
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

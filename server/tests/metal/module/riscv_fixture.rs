use std::{fs, path::Path};

pub fn build_roots(program: &Path, target: &str, output: &Path) {
    let source = fs::read_to_string(program).unwrap();
    for (interface, mode) in [("sv39-root", 8), ("sv48-root", 9), ("sv57-root", 10)] {
        let code = source.replace("ROOT_MODE", &mode.to_string());
        crate::fixture::build_module(
            code,
            target,
            &output.join(format!("{interface}.elf")),
            interface,
            "module_riscv_root",
            2,
        );
    }
}

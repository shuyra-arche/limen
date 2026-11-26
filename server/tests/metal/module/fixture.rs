use std::{
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const ENTRY: u64 = 0x1000;

pub fn build(probe: &Path, target: &str, output: &Path) {
    let rustc = env::var_os("RUSTC").unwrap();
    let host = env::var("HOST").unwrap();
    let sysroot = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .unwrap();
    assert!(sysroot.status.success());
    let sysroot = String::from_utf8(sysroot.stdout).unwrap();
    let linker = PathBuf::from(sysroot.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin")
        .join(format!("rust-lld{}", env::consts::EXE_SUFFIX));

    let source = output.with_extension("rs");
    let object = output.with_extension("o");
    let mut code = fs::read_to_string(probe).unwrap();
    let description = description();
    write!(
        code,
        "\nglobal_asm!(r#\"\n.section .note.shuyra.module, \"a\", %note\n\
         .balign 4\n.long 7, {}, 1\n.asciz \"SHUYRA\"\n.balign 4\n",
        description.len(),
    )
    .unwrap();
    for chunk in description.chunks(16) {
        let mut bytes = chunk.iter();
        write!(code, ".byte {}", bytes.next().unwrap()).unwrap();
        for byte in bytes {
            write!(code, ", {byte}").unwrap();
        }
        code.push('\n');
    }
    code.push_str(".balign 4\n\"#);\n");
    fs::write(&source, code).unwrap();

    run(Command::new(rustc)
        .args([
            "--crate-name",
            "module_probe",
            "--edition=2024",
            "--crate-type=lib",
            "--emit=obj",
            "--target",
            target,
            "-Crelocation-model=pic",
            "-Dwarnings",
            "-o",
        ])
        .arg(&object)
        .arg(&source));
    run(Command::new(linker)
        .args([
            "-flavor",
            "gnu",
            "-shared",
            "-Bsymbolic",
            "--no-undefined",
            "-z",
            "separate-loadable-segments",
            "-z",
            "noexecstack",
            "-z",
            "norelro",
            "-z",
            "text",
            "-z",
            "max-page-size=4096",
            "-e",
            "module_geometry",
            "-o",
        ])
        .arg(output)
        .arg(object));

    // The FDT declares the independently linked virtual address, before
    // placement. Keep a toolchain layout change from producing a bad fixture.
    let bytes = fs::read(output).unwrap();
    assert_eq!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()), ENTRY);
}

fn run(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
}

#[derive(Default)]
struct Fdt {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Fdt {
    fn word(&mut self, value: u32) {
        self.structure.extend(value.to_be_bytes());
    }

    fn record(&mut self, bytes: &[u8]) {
        self.structure.extend(bytes);
        self.structure
            .resize(self.structure.len().next_multiple_of(4), 0);
    }

    fn node(&mut self, name: &[u8]) {
        self.word(1);
        self.record(name);
    }

    fn property(&mut self, name: &[u8], bytes: &[u8]) {
        self.word(3);
        self.word(bytes.len().try_into().unwrap());
        self.word(self.strings.len().try_into().unwrap());
        self.strings.extend(name);
        self.record(bytes);
    }

    fn finish(mut self) -> Vec<u8> {
        self.word(9);
        let structure = 56;
        let strings = structure + self.structure.len();
        let total = strings + self.strings.len();
        let mut bytes = Vec::with_capacity(total);
        for word in [
            0xd00d_feed,
            total,
            structure,
            strings,
            40,
            17,
            16,
            0,
            self.strings.len(),
            self.structure.len(),
        ] {
            bytes.extend(u32::try_from(word).unwrap().to_be_bytes());
        }
        bytes.extend([0; 16]);
        bytes.extend(self.structure);
        bytes.extend(self.strings);
        bytes
    }
}

fn description() -> Vec<u8> {
    let mut fdt = Fdt::default();
    fdt.node(b"\0");
    fdt.property(b"version\0", &1_u32.to_be_bytes());
    fdt.property(b"abi\0", &1_u32.to_be_bytes());
    fdt.node(b"interfaces\0");
    fdt.node(b"mmu-geometry\0");
    fdt.property(b"compatible\0", b"shuyra,mmu-geometry\0");
    fdt.property(b"version\0", &1_u32.to_be_bytes());
    fdt.property(b"entry\0", &ENTRY.to_be_bytes());
    fdt.word(2);
    fdt.word(2);
    fdt.word(2);
    fdt.finish()
}

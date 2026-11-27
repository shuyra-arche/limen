//! Observe an actual terminal fault during the production BSS clear.
//! The linked ELF is never modified. GDB changes only the running guest.

use super::{diagnostic, observation};
use kernel_verification::{evidence, machine, native, platform, qmp};
use object::{Architecture, Object, ObjectSection, ObjectSymbol};
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn receipt(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn linked_guest(root: &Path, evidence: &Path) -> PathBuf {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root).args([
        "-Z",
        "panic-abort-tests",
        "test",
        "--locked",
        "--manifest-path",
        "server/tests/target/Cargo.toml",
        "-p",
        "kernel-guest",
        "--test",
        "boot-handoff",
        "--target",
        "riscv64gc-unknown-none-elf",
        "--no-run",
        "--message-format=json",
    ]);
    match env::var("LIMEN_TEST_PROFILE").unwrap().as_str() {
        "debug" => {}
        "release" => {
            command.arg("--release");
        }
        _ => panic!("unsupported guest profile"),
    }
    let output = command.output().unwrap();
    fs::write(evidence.join("cargo.stdout"), &output.stdout).unwrap();
    fs::write(evidence.join("cargo.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "boot fault guest failed to compile"
    );
    let artifacts: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|message| {
            message["reason"] == "compiler-artifact"
                && message["target"]["name"] == "boot-handoff"
                && message["profile"]["test"] == true
        })
        .filter_map(|message| message["executable"].as_str().map(PathBuf::from))
        .collect();
    assert_eq!(artifacts.len(), 1, "expected the exact linked test image");
    artifacts[0].canonicalize().unwrap()
}

const WITNESS: u64 = 0x1234_5678_9abc_def0;

#[derive(Clone, Copy)]
struct Layout {
    entry: u64,
    text_start: u64,
    text_end: u64,
    bss_start: u64,
    bss_end: u64,
    major_top: u64,
    image_start: u64,
    image_end: u64,
}

impl Layout {
    fn read(image: &Path) -> Self {
        let bytes = fs::read(image).unwrap();
        let file = object::File::parse(bytes.as_slice()).unwrap();
        assert_eq!(file.architecture(), Architecture::Riscv64);
        let symbol = |name: &str| {
            let mut matches = file
                .symbols()
                .filter(|symbol| symbol.name().ok() == Some(name) && !symbol.is_undefined());
            let address = matches.next().unwrap().address();
            assert!(matches.next().is_none(), "ambiguous linked symbol: {name}");
            address
        };
        let text = file.section_by_name(".text").unwrap();
        let layout = Self {
            entry: symbol("_start"),
            text_start: text.address(),
            text_end: text.address().checked_add(text.size()).unwrap(),
            bss_start: symbol("_bss_start"),
            bss_end: symbol("_bss_end"),
            major_top: symbol("_major_stack_end"),
            image_start: symbol("_kernel_base"),
            image_end: symbol("_kernel_edge"),
        };
        let major_start = symbol("_major_stack_start");
        assert!((layout.text_start..layout.text_end).contains(&layout.entry));
        assert!(layout.bss_end.checked_sub(layout.bss_start).unwrap() > 8);
        assert!(layout.bss_start.is_multiple_of(8) && layout.bss_end.is_multiple_of(8));
        assert!(
            layout.bss_start <= major_start
                && major_start < layout.major_top
                && layout.major_top <= layout.bss_end
        );
        assert!(major_start.is_multiple_of(16) && layout.major_top.is_multiple_of(16));
        assert!(layout.image_start <= layout.text_start && layout.bss_end <= layout.image_end);
        layout
    }

    fn commands(&self) -> String {
        // GDB controls execution and reports raw observations; Rust owns the
        // final contract. No instruction bytes or instruction offsets locate
        // the BSS clear interval.
        let Self {
            entry,
            text_start,
            text_end,
            bss_start,
            bss_end,
            major_top,
            image_start,
            image_end,
        } = *self;
        let last = bss_end - 8;
        format!(
            r#"set confirm off
set pagination off
set remotetimeout 10
set language c
set architecture riscv:rv64
target remote ./gdb.sock
hbreak *{entry:#x}
continue
if (unsigned long)$pc != {entry:#x}
  echo unexpected boot entry\n
  quit 1
end
delete breakpoints
set {{unsigned long long}}{bss_start:#x} = {WITNESS:#x}
set {{unsigned long long}}{last:#x} = {WITNESS:#x}
watch -location *(unsigned long long *){bss_start:#x}
continue
delete breakpoints
if *(unsigned long long *){bss_start:#x} == {WITNESS:#x}
  stepi
end
if *(unsigned long long *){bss_start:#x} != 0 || *(unsigned long long *){last:#x} != {WITNESS:#x}
  echo BSS witnesses do not establish a partial clear\n
  quit 1
end
# GDB exposes PC and SP as pointer values. Normalize every address before
# arithmetic, so these checks do not depend on a debugger's register types.
set $fault_pc = (unsigned long)$pc
set $saved_sp = (unsigned long)$sp
set $trap_vector = (unsigned long)$stvec
set $claim = (unsigned long)$sscratch
if $fault_pc < {text_start:#x} || $fault_pc + 2 > {text_end:#x} || ($fault_pc & 1) != 0 || $saved_sp != {major_top:#x}
  echo unexpected interrupted PC or stack\n
  quit 1
end
if $trap_vector < {text_start:#x} || $trap_vector >= {text_end:#x} || ($trap_vector & 3) != 0
  echo invalid early trap vector\n
  quit 1
end
if $claim < {image_start:#x} || $claim + 8 > {image_end:#x} || ($claim & 7) != 0
  echo invalid emergency stack claim address\n
  quit 1
end
if $claim < {bss_end:#x} && $claim + 8 > {bss_start:#x}
  echo emergency stack claim overlaps BSS\n
  quit 1
end
printf "LIMEN_CLEAR %lx %lx %lx %lx %lx %lx %lx\n", $fault_pc, $saved_sp, $trap_vector, $claim, *(unsigned long *)$claim, *(unsigned long *){bss_start:#x}, *(unsigned long *){last:#x}
set {{unsigned short}}$fault_pc = 0
if *(unsigned short *)$fault_pc != 0
  echo fault injection did not reach guest memory\n
  quit 1
end
hbreak *$trap_vector
continue
delete breakpoints
printf "LIMEN_TRAP %lx %lx %lx %lx %lx %lx\n", (unsigned long)$pc, (unsigned long)$scause, (unsigned long)$sepc, (unsigned long)$stval, (unsigned long)$sp, (unsigned long)$sstatus
detach
"#
        )
    }
}

fn fault_observation_error(directory: &Path, error: impl std::fmt::Display) -> ! {
    let read = |name: &str| fs::read_to_string(directory.join(name)).unwrap_or_default();
    panic!(
        "{error}\nGDB stdout:\n{}\nGDB stderr:\n{}\nQMP:\n{}\nMachine process:\n{}",
        read("controller/serial.log"),
        read("controller/stderr.log"),
        read("qmp.jsonl"),
        read("process.json"),
    )
}

#[test]
fn bss_clear_traps_on_the_original_major_stack() {
    use std::os::unix::fs::PermissionsExt;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let evidence = root.join(".integration/boot-fault");
    evidence::fresh(&evidence).unwrap();
    let image = linked_guest(&root, &evidence);
    let hash = evidence::digest(&image).unwrap();
    let platform = platform::read(&image).unwrap();
    assert_eq!(platform.architecture, "riscv64");
    assert_eq!(platform.platform.harts, 4);

    // The normal run and the fault run use the same actual Cargo artifact.
    let normal = native::run(&image, false, Some(&evidence.join("normal"))).unwrap();
    let result = receipt(&normal.join("result.json"));
    assert_eq!(result["verdict"], "pass");
    let process = receipt(&normal.join("process.json"));
    assert_eq!(process["termination"]["kind"], "exited");
    assert_eq!(process["termination"]["value"], 97);
    assert_eq!(process["cleanup"]["settled"], true);
    assert_eq!(process["cleanup"]["unexpected_descendants"], false);
    assert!(process["cleanup"]["error"].is_null());

    let layout = Layout::read(&image);
    let fault = evidence.join("fault");
    evidence::fresh(&fault).unwrap();
    fs::set_permissions(&fault, fs::Permissions::from_mode(0o700)).unwrap();
    let mut machine = machine::prepare(&image, &fault, true).unwrap();
    machine.current_dir(&fault).args([
        "-chardev",
        "socket,path=gdb.sock,server=on,wait=off,id=debugger",
        "-gdb",
        "chardev:debugger",
    ]);
    fs::write(fault.join("control.gdb"), layout.commands()).unwrap();
    let mut controller =
        Command::new(env::var_os("GDB_MULTIARCH").unwrap_or_else(|| "gdb-multiarch".into()));
    controller
        .current_dir(&fault)
        .args(["-nx", "-batch", "-se"])
        .arg(&image)
        .args(["-x", "control.gdb"]);
    let (result, shutdown) =
        qmp::run_isolated(machine, &fault, Duration::from_secs(45), Some(controller))
            .unwrap_or_else(|error| fault_observation_error(&fault, error));
    let shutdown = shutdown.unwrap_or_else(|error| fault_observation_error(&fault, error));
    assert!(result.success(), "{result:?}");
    assert!(shutdown.guest);
    assert_eq!(shutdown.reason, "guest-shutdown");

    let observed = fs::read_to_string(fault.join("controller/serial.log")).unwrap();
    let [pc, stack, vector, scratch, emergency, first, last] =
        observation(&observed, "LIMEN_CLEAR");
    assert!((layout.text_start..layout.text_end).contains(&pc));
    assert_eq!(stack, layout.major_top);
    assert!((layout.text_start..layout.text_end).contains(&vector));
    assert!(vector.is_multiple_of(4));
    assert!(scratch.is_multiple_of(8));
    assert!(layout.image_start <= scratch && scratch.checked_add(8).unwrap() <= layout.image_end);
    assert!(scratch + 8 <= layout.bss_start || scratch >= layout.bss_end);
    assert!(emergency.is_multiple_of(16));
    assert!((layout.image_start..=layout.image_end).contains(&emergency));
    assert_eq!(first, 0);
    assert_eq!(last, WITNESS);

    let [entered, cause, interrupted, value, saved_stack, status] =
        observation(&observed, "LIMEN_TRAP");
    assert_eq!(entered, vector);
    assert_eq!(cause, 2);
    assert_eq!(interrupted, pc);
    assert_eq!(value, 0);
    assert_eq!(saved_stack, stack);
    let channels = ["serial.log", "semihost.log", "stdout.log"]
        .map(|name| fs::read_to_string(fault.join(name)).unwrap());
    diagnostic(
        &channels.each_ref().map(String::as_str),
        cause,
        interrupted,
        value,
        saved_stack,
        status,
    );
    assert_eq!(evidence::digest(&image).unwrap(), hash);
    evidence::json(
        &fault.join("observation.json"),
        &serde_json::json!({
            "image_sha256": hash,
            "bss_start": layout.bss_start,
            "bss_end": layout.bss_end,
            "first": first,
            "last": last,
            "pc": interrupted,
            "stack": saved_stack,
            "cause": cause,
            "value": value,
            "status": status,
            "vector": entered,
            "scratch": scratch,
            "emergency_top": emergency,
        }),
    )
    .unwrap();
}

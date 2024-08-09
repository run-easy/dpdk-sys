use std::env;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::str;
use std::sync::LazyLock;
use std::sync::OnceLock;

// On Ubuntu server, we need the following packages:
// 1. meson (apt install meson) for meson build
// 2. pyelf-tool (apt install python3-pyelftools) for meson configuration
// 3. clang (apt install clang) for bindgen
// 4. libnuma-dev (apt install libnuma-dev) for NUMA support

static DPDK_LIBS: LazyLock<Vec<DpdkLib>> = LazyLock::new(|| {
    let dpdk_map = std::fs::File::open("dpdk.map").expect("Failed to open dpdk.map");
    let mut buf = String::new();
    let mut r = BufReader::new(dpdk_map);
    r.read_to_string(&mut buf).unwrap();
    let dpdk_libs = DpdkLib::parse_from(buf.as_bytes()).unwrap();
    dpdk_libs
});

static MESON_VERSION: &'static str = "0.53.2";
static DPDK_VERSION: &'static str = "23.11.1";
static CUREENT_DIR: OnceLock<PathBuf> = OnceLock::new();
static DOWNLOAD_URL: LazyLock<String> =
    LazyLock::new(|| format!("https://fast.dpdk.org/rel/dpdk-{}.tar.xz", DPDK_VERSION));
static MD5SUM: &'static str = "382d5fdd8ecb1d8e0be6d70dfc5eec96";

static SOURCE_DIR: &'static str = "deps/src";

static BUILD_DIR: &'static str = "deps/build";

static INSTALL_DIR: &'static str = "deps/install";

static DPDK_CFLAGS: OnceLock<Vec<String>> = OnceLock::new();

static DPDK_LINK_OPTIONS: OnceLock<Vec<String>> = OnceLock::new();

fn main() {
    CUREENT_DIR.get_or_init(|| std::fs::canonicalize("./").unwrap());

    let mut force = match std::env::var("FORCE")
        .unwrap_or(String::from("false"))
        .to_ascii_lowercase()
        .as_str()
        .trim()
    {
        "yes" | "y" | "true" | "on" | "1" => true,
        _ => false,
    };

    if force || !check_step("download") {
        download();
        force = true;
    }

    if force || !check_step("configure") {
        configure();
        force = true;
    }

    if force || !check_step("build") {
        build();
        force = true;
    }

    if force || !check_step("install") {
        install();
    }

    generate_library();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=dpdk.map");
    println!("cargo:rerun-if-changed=csrc/impl.c");
    println!("cargo:rerun-if-changed=csrc/header.h");
}

fn install() {
    std::fs::remove_file("deps/install.ok").unwrap_or_default();

    let output = Command::new("ninja")
        .args(["-C", BUILD_DIR, "install"])
        .output()
        .expect("Please install ninja");
    if !output.status.success() {
        panic!(
            "Failed to install dpdk, stderr: {}",
            String::from_utf8(output.stderr).unwrap()
        );
    }

    std::fs::File::create("deps/install.ok").unwrap();
}

fn build() {
    std::fs::remove_file("deps/build.ok").unwrap_or_default();

    let output = Command::new("ninja")
        .args(["-C", BUILD_DIR])
        .output()
        .expect("Please install ninja");
    if !output.status.success() {
        panic!(
            "Failed to build dpdk, stderr: {}",
            String::from_utf8(output.stderr).unwrap()
        );
    }

    std::fs::File::create("deps/build.ok").unwrap();
}

fn configure() {
    std::fs::remove_file("deps/configure.ok").unwrap_or_default();

    let meson_version = String::from_utf8(
        Command::new("meson")
            .arg("--version")
            .output()
            .expect("Please install meson.")
            .stdout,
    )
    .unwrap();

    if meson_version.trim() < MESON_VERSION {
        panic!(
            "Failed to configure dpdk, meson version `{}` < `{}`",
            meson_version, MESON_VERSION
        );
    }

    let result = Command::new("meson")
        .args([
            "setup",
            "--wipe",
            "--prefix",
            CUREENT_DIR
                .get()
                .unwrap()
                .join(INSTALL_DIR)
                .to_str()
                .unwrap(),
            BUILD_DIR,
            SOURCE_DIR,
        ])
        .output()
        .expect("Please install meson");

    if !result.status.success() {
        let err = String::from_utf8(result.stdout).unwrap();
        panic!("Failed to configure dpdk, stderr: {}", err);
    }

    std::fs::File::create("deps/configure.ok").expect("Failed to create deps/configure.ok");
}

fn download() {
    std::fs::remove_file("deps/download.ok").unwrap_or_default();

    let dpdk_file = format!("dpdk-{}.tar.xz", DPDK_VERSION);
    #[allow(unused_assignments)]
    let mut succ = false;

    if !succ {
        if let Ok(result) = Command::new("wget")
            .current_dir("deps")
            .args(["-O", dpdk_file.as_str(), DOWNLOAD_URL.as_str()])
            .status()
        {
            if result.success() {
                std::fs::File::create(format!("deps/download.ok"))
                    .expect("Failed to create deps/download.ok");
                succ = true;
            } else {
                panic!(
                    "Failed to download dpdk {} from {}",
                    DPDK_VERSION,
                    DOWNLOAD_URL.as_str()
                );
            }
        }
    }

    if !succ {
        let result = Command::new("curl")
            .current_dir("deps")
            .args(["-s", "-o", dpdk_file.as_str(), DOWNLOAD_URL.as_str()])
            .status()
            .expect("Please install curl or wget");

        if result.success() {
            std::fs::File::create(format!("deps/download.ok"))
                .expect("Failed to create deps/download.ok");
        } else {
            panic!(
                "Failed to download dpdk {} from {}",
                DPDK_VERSION,
                DOWNLOAD_URL.as_str()
            );
        }
    }

    // succ = true;

    if succ {
        let file = std::fs::File::open(format!("deps/{}", dpdk_file)).expect("Failed to open file");
        let md5sum = chksum_md5::chksum(file).expect("Failed to calculate md5sum");
        if md5sum.to_hex_lowercase() != MD5SUM.to_lowercase() {
            panic!("MD5 checksum failed");
        }

        let result = Command::new("tar")
            .current_dir("deps")
            .args(["-xf", dpdk_file.as_str()])
            .status()
            .expect("Please install tar");
        if !result.success() {
            panic!("Failed to uncompress {}", dpdk_file);
        }

        // rename to deps/src
        let origin_source_dir = {
            let dir = PathBuf::from(format!(
                "deps/dpdk-{}",
                DPDK_VERSION
                    .strip_suffix(".0")
                    .map(|s| s.to_string())
                    .unwrap_or(format!("{}", DPDK_VERSION))
            ));
            if dir.exists() && dir.is_dir() {
                dir
            } else {
                PathBuf::from(format!(
                    "deps/dpdk-stable-{}",
                    DPDK_VERSION
                        .strip_suffix(".0")
                        .map(|s| s.to_string())
                        .unwrap_or(format!("{}", DPDK_VERSION))
                ))
            }
        };

        if !origin_source_dir.exists() || !origin_source_dir.is_dir() {
            panic!(
                "Cannot find downloaded dpdk package `{}`",
                origin_source_dir.to_str().unwrap()
            );
        }

        let source_dir = PathBuf::from(SOURCE_DIR);
        if source_dir.exists() {
            if source_dir.is_dir() {
                std::fs::remove_dir_all(source_dir).expect("Failed to remove deps/src");
            } else if source_dir.is_file() {
                std::fs::remove_file(source_dir).expect("Failed to remove deps/src");
            } else {
                panic!("deps/src is a symbol link. manually remove it");
            }
        }

        std::fs::rename(&origin_source_dir, SOURCE_DIR).expect(
            format!(
                "Failed to rename {} to {}",
                origin_source_dir.to_str().unwrap(),
                SOURCE_DIR
            )
            .as_str(),
        );

        std::fs::File::create("deps/download.ok").expect("Failed to create deps/download.ok");
    }
}

fn check_step(step: &'static str) -> bool {
    let step_ok = std::path::PathBuf::from(format!("deps/{}.ok", step));
    if step_ok.exists() && step_ok.is_file() {
        return true;
    }
    return false;
}

fn generate_library() {
    pkgconfig();
    add_module(
        [
            "eal",
            "lcore",
            "mbuf",
            "mempool",
            "ethdev",
            "build_config",
            "config",
            "errno",
        ],
        "eal",
    );
    add_module(["power"], "power");
    link_dpdk();
}

fn add_module<S: AsRef<str>, I: IntoIterator<Item = S>>(dpdk_libs: I, module_name: &'static str) {
    let mut selected_libs = vec![];
    for lib in dpdk_libs {
        let lib = lib.as_ref();
        let match_libs = DPDK_LIBS
            .iter()
            .filter(|dpdk_lib| dpdk_lib.name.as_str() == lib)
            .collect::<Vec<&DpdkLib>>();
        if match_libs.len() == 0 {
            panic!("{} not found in dpdk.map", lib);
        }
        selected_libs.push(match_libs[0].clone());
    }

    DpdkLib::build(selected_libs, module_name);
}

fn pkgconfig() {
    let mut pkg_config_path = env::var("PKG_CONFIG_PATH").unwrap_or_default();
    if pkg_config_path.is_empty() {
        pkg_config_path = CUREENT_DIR
            .get()
            .unwrap()
            .join(format!("deps/install/lib/x86_64-linux-gnu/pkgconfig"))
            .join(":/usr/lib/x86_64-linux-gnu/pkgconfig")
            .to_str()
            .unwrap()
            .to_string();
    } else {
        pkg_config_path = CUREENT_DIR
            .get()
            .unwrap()
            .join(format!(
                "deps/dpdk-stable-{}-install/lib/x86_64-linux-gnu/pkgconfig",
                DPDK_VERSION
            ))
            .join(":/usr/lib/x86_64-linux-gnu/pkgconfig")
            .join(format!(":{}", pkg_config_path))
            .to_str()
            .unwrap()
            .to_string();
    }

    // Set PKG_CONFIG_PATH environment variable to point to the installed DPDK library.
    env::set_var("PKG_CONFIG_PATH", pkg_config_path.as_str());

    let output = Command::new("pkg-config")
        .args(&["--modversion", "libdpdk"])
        .output()
        .expect("Please install pkg-config.");
    if !output.status.success() {
        panic!(
            "Failed to find dpdk cflags. DPDK is not successfully installed by the build script."
        )
    }

    // check dpdk version
    let s = String::from_utf8(output.stdout).unwrap();
    let version_str = s.trim();
    if !version_str.starts_with(DPDK_VERSION) {
        panic!(
            "pkg-config finds another DPDK library with version {}.",
            version_str
        );
    }

    let _ = DPDK_CFLAGS.get_or_init(|| {
        // Probe the cflags of the installed DPDK library.
        let output = Command::new("pkg-config")
            .args(&["--cflags", "libdpdk"])
            .output()
            .unwrap();
        assert!(output.status.success() == true);
        let cflags = String::from_utf8(output.stdout).unwrap();
        cflags
            .trim()
            .split(' ')
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    });

    let _ = DPDK_LINK_OPTIONS.get_or_init(|| {
        let output = Command::new("pkg-config")
            .args(&["--libs", "--static", "libdpdk"])
            .output()
            .unwrap();

        assert!(output.status.success() == true);

        let ldflags = String::from_utf8(output.stdout).unwrap();
        ldflags
            .trim()
            .split(' ')
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    });
}

fn link_dpdk() {
    let mut cbuild = cc::Build::new();
    cbuild.opt_level(3);
    for cflag in DPDK_CFLAGS.get().unwrap().iter() {
        cbuild.flag(cflag);
    }
    cbuild.file("csrc/impl.c").compile("impl");

    for ldflag in DPDK_LINK_OPTIONS.get().unwrap().iter() {
        if ldflag.starts_with("-L") {
            println!("cargo:rustc-link-search=native={}", &ldflag[2..]);
        } else if ldflag.starts_with("-l") {
            if ldflag.ends_with(".a") {
                if !ldflag.starts_with("-l:lib") {
                    panic!("Invalid linker option: {}", ldflag);
                }
                let end_range = ldflag.len() - 2;
                println!(
                    "cargo:rustc-link-lib=static:+whole-archive,-bundle={}",
                    &ldflag[6..end_range]
                );
            } else {
                if !ldflag.starts_with("-lrte") {
                    println!("cargo:rustc-link-lib={}", &ldflag[2..]);
                }
            }
        } else {
            if ldflag == "-pthread" {
                println!("cargo:rustc-link-lib={}", &ldflag[1..]);
            } else if ldflag.starts_with("-Wl") {
                // We do nothing with -Wl linker options.
            } else {
                panic!("Invalid linker option: {}.", ldflag);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct DpdkLib {
    name: String,
    functions: Vec<String>,
    vars: Vec<String>,
    types: Vec<String>,
}

impl DpdkLib {
    fn build(libs: Vec<Self>, module: &'static str) {
        let mut bgbuilder = bindgen::builder()
            .generate_inline_functions(true)
            .header("csrc/header.h");
        let mut libs = libs;
        for lib in libs.drain(..) {
            for function in lib.functions.iter() {
                bgbuilder = bgbuilder.allowlist_function(function);
            }

            for var in lib.vars.iter() {
                bgbuilder = bgbuilder.allowlist_var(var);
            }

            for t in lib.types.iter() {
                bgbuilder = bgbuilder.allowlist_type(t);
            }
        }

        let cflags: Vec<&str> = DPDK_CFLAGS
            .get()
            .unwrap()
            .iter()
            .map(|s| s.as_str())
            .collect();

        bgbuilder
            .clang_args(cflags)
            .generate()
            .unwrap()
            .write_to_file(format!("src/{}.rs", module))
            .unwrap();

        Self::add_module(module);
    }

    fn add_module(name: &str) {
        static INIT: OnceLock<()> = OnceLock::new();

        let mut first = false;
        let f = if INIT.get().is_none() {
            INIT.get_or_init(|| {});
            first = true;
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open("src/lib.rs")
                .unwrap()
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .create(false)
                .open("src/lib.rs")
                .unwrap()
        };

        let mut w = BufWriter::new(f);

        if first {
            w.write_all(
                b"#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]",
            )
            .unwrap();
        }

        w.write_all(
            format!(
                "
#[cfg(feature = \"{}\")]
mod {};
#[cfg(feature = \"{}\")]
pub use {}::*;
        ",
                name, name, name, name
            )
            .as_bytes(),
        )
        .unwrap();
    }

    fn clear(&mut self) {
        self.name.clear();
        self.functions.clear();
        self.vars.clear();
        self.types.clear();
    }

    fn is_clear(&self) -> bool {
        self.name.is_empty()
            && self.functions.is_empty()
            && self.vars.is_empty()
            && self.types.is_empty()
    }

    fn parse_from(value: &[u8]) -> Result<Vec<Self>, String> {
        if !value.is_ascii() {
            return Err("Contains invalid ASCII characters".to_string());
        }

        let mut ret = vec![];

        let mut lib = Self {
            name: String::new(),
            functions: Vec::new(),
            vars: Vec::new(),
            types: Vec::new(),
        };

        let mut state = 0;
        let mut lines = 1;
        let mut offset = 0;
        let mut field = String::new();
        let mut ident = String::new();
        let mut delimiter = String::new();

        // assert_eq!(delimiter, ' ');

        for (_pos, c) in value.iter().enumerate() {
            let ch = char::from_u32(*c as u32).unwrap();

            if ch == '\n' {
                lines += 1;
                offset = 0;
            }

            offset += 1;

            match state {
                // parsing name
                0 => {
                    if ch == ' ' {
                        continue;
                    }

                    if lib.name.is_empty() && ch == '\n' {
                        continue;
                    }

                    if !lib.name.is_empty() && ch == '{' {
                        delimiter.clear();
                        delimiter.push(ch);
                        state = 3;
                        continue;
                    }

                    if ch.is_ascii_alphabetic()
                        || ch == '_'
                        || ch == '-'
                        || ch.is_ascii_alphanumeric()
                    {
                        lib.name.push(ch);
                        continue;
                    }

                    return Err(format!(
                        "Invalid map format at offset {} of line {}",
                        offset, lines
                    ));
                }
                1 => {
                    if ch == ' ' {
                        continue;
                    }

                    if ch == ':' {
                        delimiter.clear();
                        delimiter.push(ch);
                        state = 3;
                        continue;
                    }

                    field.push(ch);
                    continue;
                }
                2 => {
                    if ch == ' ' {
                        continue;
                    }

                    if ch == ';' {
                        delimiter.clear();
                        delimiter.push(ch);
                        state = 3;
                        continue;
                    }

                    ident.push(ch);
                }
                3 => match delimiter.as_str() {
                    "{" => {
                        if ch == ' ' {
                            continue;
                        }

                        if ch != '\n' {
                            return Err(format!("Invalid delimiter {{{}", ch));
                        } else {
                            field.clear();
                            delimiter.clear();
                            state = 1;
                        }
                    }
                    ":" => {
                        if ch == ' ' {
                            continue;
                        }

                        if ch != '\n' {
                            return Err(format!("Invalid delimiter {{{}", ch));
                        } else {
                            delimiter.push(ch);
                        }
                    }
                    ":\n" => {
                        if ch == '\n' {
                            delimiter.clear();
                            match field.as_str() {
                                "function" | "var" | "type" => {}
                                _ => {
                                    return Err(format!(
                                        "Unknown field {} at line {}",
                                        field, lines
                                    ))
                                }
                            }
                            state = 2;
                        } else if ch == ' ' {
                            continue;
                        } else {
                            return Err(format!(
                                "Fields and identifiers must be separated by blank lines"
                            ));
                        }
                    }
                    ";" => {
                        if ch == ' ' {
                            continue;
                        }

                        if ch != '\n' {
                            return Err(format!(
                                "Invalid delimeter at offset {} of line {}",
                                offset, lines
                            ));
                        } else {
                            delimiter.push(ch);
                        }
                    }
                    ";\n" => {
                        if ch == ' ' {
                            continue;
                        }

                        if ch == '\n' {
                            match field.as_str() {
                                "function" => {
                                    lib.functions.push(ident.clone());
                                }
                                "var" => {
                                    lib.vars.push(ident.clone());
                                }
                                "type" => {
                                    lib.types.push(ident.clone());
                                }
                                _ => unreachable!(),
                            }
                            field.clear();
                            ident.clear();
                            state = 1;
                            continue;
                        }

                        if ch == '}' {
                            delimiter.clear();
                            delimiter.push(ch);
                        }

                        match field.as_str() {
                            "function" => {
                                lib.functions.push(ident.clone());
                            }
                            "var" => {
                                lib.vars.push(ident.clone());
                            }
                            "type" => {
                                lib.types.push(ident.clone());
                            }
                            _ => unreachable!(),
                        }

                        ident.clear();

                        if ch != '}' {
                            ident.push(ch);
                            delimiter.clear();
                            state = 2;
                        }
                    }
                    "}" => {
                        if ch != ';' {
                            return Err(format!("Unknown delimeter ;{} at line {}", ch, lines));
                        } else {
                            state = 0;
                            ret.push(lib.clone());
                            lib.clear();
                        }
                    }
                    _ => return Err(format!("Unknown delimeter {} at line {}", delimiter, lines)),
                },
                _ => unreachable!(),
            }
        }

        if !lib.is_clear() {
            return Err(format!("There is no exhaustive map"));
        }

        Ok(ret)
    }
}

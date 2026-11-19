//! Builds the OpenColorIO shim and links OpenColorIO statically.
//!
//! Where OCIO comes from, in order:
//! 1. `PHOTOCRAFT_NO_OCIO=1` (or a wasm target): no OCIO; a stub with the same API is compiled.
//! 2. `OCIO_ROOT`: an existing install prefix (`include/`, `lib/`), for custom builds. Extra
//!    library folders for its dependencies can be listed in `OCIO_EXTRA_LIB_DIRS`.
//! 3. `VCPKG_ROOT`: vcpkg builds the version pinned in the repo's `vcpkg.json` into
//!    `vcpkg_installed/` at the workspace root (first build only), then it is linked from there.
//!
//! Otherwise the build stops with instructions. The linked version must be `PINNED`, so
//! results can't drift from Nuke's (`PHOTOCRAFT_OCIO_ANY_VERSION=1` skips the check).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Keep in sync with `PINNED_VERSION` in src/lib.rs and `vcpkg.json`.
const PINNED: &str = "2.4.2";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(ocio_stub)");
    for var in ["PHOTOCRAFT_NO_OCIO", "OCIO_ROOT", "OCIO_EXTRA_LIB_DIRS", "VCPKG_ROOT", "PHOTOCRAFT_OCIO_ANY_VERSION", "VCPKG_DEFAULT_TRIPLET"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    println!("cargo::rerun-if-changed=src/shim.cpp");

    let target = env::var("TARGET").unwrap_or_default();
    if env::var("PHOTOCRAFT_NO_OCIO").is_ok_and(|v| v == "1") || target.starts_with("wasm32") {
        if !target.starts_with("wasm32") {
            println!("cargo::warning=photocraft-ocio: built without OpenColorIO (PHOTOCRAFT_NO_OCIO=1); colorspace features are disabled");
        }
        println!("cargo::rustc-cfg=ocio_stub");
        return;
    }

    let (prefix, mut lib_dirs) = match env::var_os("OCIO_ROOT") {
        Some(root) => (PathBuf::from(root), Vec::new()),
        None => match env::var_os("VCPKG_ROOT") {
            Some(vcpkg) => (vcpkg_prefix(Path::new(&vcpkg), &target), Vec::new()),
            None => fail_missing(),
        },
    };
    lib_dirs.insert(0, prefix.join("lib"));
    if let Some(extra) = env::var_os("OCIO_EXTRA_LIB_DIRS") {
        lib_dirs.extend(env::split_paths(&extra));
    }
    let include = prefix.join("include");
    check_version(&include);

    let windows = target.contains("windows");
    let mut build = cc::Build::new();
    build.cpp(true).file("src/shim.cpp").include(&include).warnings(false);
    if target.contains("msvc") {
        build.flag("/std:c++17").flag("/EHsc");
    } else {
        build.flag("-std=c++17");
    }
    if windows {
        // Static OpenColorIO: no dllimport.
        build.define("OpenColorIO_SKIP_IMPORTS", None);
    }
    build.compile("photocraft_ocio_shim");

    for dir in &lib_dirs {
        println!("cargo::rustc-link-search=native={}", dir.display());
    }
    for lib in static_libs(&lib_dirs, windows) {
        println!("cargo::rustc-link-lib=static={lib}");
    }
    if windows {
        // Static OpenColorIO's Windows system libraries: monitor queries (SystemMonitor.cpp:
        // gdi32, user32) and minizip-ng's AES / HMAC (bcrypt).
        for lib in ["gdi32", "user32", "bcrypt"] {
            println!("cargo::rustc-link-lib=dylib={lib}");
        }
    }
}

/// The vcpkg triplet for `target` (static libraries, the dynamic CRT on Windows like Rust's).
fn triplet(target: &str) -> String {
    if let Ok(t) = env::var("VCPKG_DEFAULT_TRIPLET") {
        return t;
    }
    let arch = if target.starts_with("aarch64") { "arm64" } else { "x64" };
    if target.contains("windows") {
        format!("{arch}-windows-static-md")
    } else if target.contains("apple") {
        format!("{arch}-osx")
    } else {
        format!("{arch}-linux")
    }
}

/// `vcpkg_installed/<triplet>` at the workspace root, installed from `vcpkg.json` if missing.
fn vcpkg_prefix(vcpkg: &Path, target: &str) -> PathBuf {
    // The workspace root, two levels up. Not `canonicalize`: on Windows that gives a `\\?\`
    // path, which MSVC can't take as an include folder.
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ws = manifest.parent().and_then(Path::parent).expect("workspace root").to_path_buf();
    println!("cargo::rerun-if-changed={}", ws.join("vcpkg.json").display());
    let triplet = triplet(target);
    let installed = ws.join("vcpkg_installed");
    let prefix = installed.join(&triplet);
    if prefix.join("include/OpenColorIO/OpenColorIO.h").exists() {
        return prefix;
    }
    println!("cargo::warning=photocraft-ocio: building OpenColorIO {PINNED} with vcpkg ({triplet}; first build only, about 10-20 minutes)");
    let exe = vcpkg.join(if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" });
    let status = Command::new(&exe)
        .arg("install")
        .arg(format!("--triplet={triplet}"))
        .arg(format!("--x-manifest-root={}", ws.display()))
        .arg(format!("--x-install-root={}", installed.display()))
        .status();
    match status {
        Ok(s) if s.success() && prefix.join("include/OpenColorIO/OpenColorIO.h").exists() => prefix,
        Ok(s) => panic!(
            "\n\n  vcpkg could not build OpenColorIO ({s}).\n  Run it yourself to see why:\n    cd {}\n    {} install --triplet={triplet}\n\n",
            ws.display(),
            exe.display()
        ),
        Err(e) => panic!("\n\n  Could not run vcpkg at {} ({e}).\n  Check that VCPKG_ROOT points at your vcpkg folder.\n\n", exe.display()),
    }
}

fn fail_missing() -> ! {
    panic!(
        "\n\n  PhotoCraft VFX needs OpenColorIO {PINNED}, built through vcpkg.\n\n  \
         1. Install vcpkg once:\n       git clone https://github.com/microsoft/vcpkg\n       \
         vcpkg\\bootstrap-vcpkg.bat        (Linux/macOS: ./vcpkg/bootstrap-vcpkg.sh)\n  \
         2. Set VCPKG_ROOT to that folder and open a new terminal.\n  \
         3. Run cargo build again; OpenColorIO {PINNED} is built automatically.\n\n  \
         To use an existing OpenColorIO install, set OCIO_ROOT to its prefix instead.\n  \
         To build without OCIO (no colorspace nodes), set PHOTOCRAFT_NO_OCIO=1.\n\n"
    )
}

fn check_version(include: &Path) {
    let abi = include.join("OpenColorIO/OpenColorABI.h");
    let Ok(text) = std::fs::read_to_string(&abi) else {
        panic!("\n\n  No OpenColorIO headers at {}.\n  Set OCIO_ROOT or VCPKG_ROOT (see crates/ocio/build.rs).\n\n", abi.display())
    };
    let found = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define OCIO_VERSION ").map(|v| v.trim().trim_matches('"').to_string()))
        .unwrap_or_default();
    if found != PINNED && !env::var("PHOTOCRAFT_OCIO_ANY_VERSION").is_ok_and(|v| v == "1") {
        panic!(
            "\n\n  Found OpenColorIO {found} at {}, but PhotoCraft VFX is pinned to {PINNED} to match Nuke.\n  \
             Use the repo's vcpkg.json (VCPKG_ROOT), or set PHOTOCRAFT_OCIO_ANY_VERSION=1 to build anyway.\n\n",
            include.display()
        );
    }
}

/// Every static library in `dirs`, OpenColorIO first, then its dependencies (GNU ld resolves
/// left to right), skipping debug builds and test/tool libraries.
fn static_libs(dirs: &[PathBuf], windows: bool) -> Vec<String> {
    let mut libs = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let file = e.file_name().to_string_lossy().into_owned();
            let name = if windows {
                file.strip_suffix(".lib").map(str::to_string)
            } else {
                file.strip_prefix("lib").and_then(|f| f.strip_suffix(".a")).map(str::to_string)
            };
            if let Some(n) = name
                && !n.to_ascii_lowercase().contains("test")
                && !libs.contains(&n)
            {
                libs.push(n);
            }
        }
    }
    // OpenColorIO, then the libraries it uses, then whatever they use (minizip → zlib).
    let rank = |n: &str| {
        let l = n.to_ascii_lowercase();
        if l.starts_with("opencolorio") {
            0
        } else if l.contains("minizip") {
            2
        } else if l.starts_with("z") || l.contains("zlib") || l.contains("bz2") || l.contains("lzma") || l.contains("zstd") {
            3
        } else {
            1
        }
    };
    libs.sort_by_key(|n| rank(n));
    libs
}

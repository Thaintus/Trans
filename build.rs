use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/trans.rc");
    println!("cargo:rerun-if-changed=assets/trans.ico");
    println!("cargo:rerun-if-changed=assets/lucide-focus-LICENSE.txt");
    println!("cargo:rerun-if-changed=assets/lucide-rotate-cw-LICENSE.txt");
    for variable in [
        "RC",
        "WindowsSdkDir",
        "WindowsSDKVersion",
        "ProgramFiles(x86)",
        "ProgramFiles",
        "PATH",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        panic!("Trans requires a Windows MSVC target");
    }

    let project_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"));
    let manifest_path = project_dir.join("assets").join("app.manifest");
    assert!(
        manifest_path.is_absolute(),
        "manifest path must be absolute"
    );
    assert!(manifest_path.is_file(), "application manifest is missing");

    let assets_dir = project_dir.join("assets");
    let resource_script = assets_dir.join("trans.rc");
    assert!(resource_script.is_file(), "icon resource script is missing");
    assert!(
        assets_dir.join("trans.ico").is_file(),
        "application icon is missing"
    );
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    assert!(
        out_dir.is_absolute(),
        "resource output path must be absolute"
    );
    let resource_output = out_dir.join("trans.res");
    let compiler = fs::canonicalize(find_resource_compiler())
        .expect("cannot resolve the Windows resource compiler path");
    let status = Command::new(&compiler)
        .current_dir(&assets_dir)
        .arg("/nologo")
        .arg("/I")
        .arg(&assets_dir)
        .arg("/fo")
        .arg(&resource_output)
        .arg(&resource_script)
        .status()
        .unwrap_or_else(|error| panic!("cannot start {}: {error}", compiler.display()));
    assert!(
        status.success(),
        "Windows icon resource compilation failed: {status}"
    );
    assert!(
        resource_output.is_file(),
        "Windows icon resource output is missing"
    );
    println!("cargo:rustc-link-arg={}", resource_output.display());

    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
        manifest_path.display()
    );
}

fn find_resource_compiler() -> PathBuf {
    if let Some(configured) = env::var_os("RC") {
        let configured = configured.to_string_lossy();
        let configured = configured.trim().trim_matches('"');
        if let Some(compiler) = resolve_executable(configured) {
            return compiler;
        }
        panic!("RC points to a missing resource compiler: {configured}");
    }

    let host = env::var("HOST").unwrap_or_default();
    let architectures: &[&str] = if host.starts_with("aarch64-") {
        &["arm64", "x64", "x86"]
    } else if host.starts_with("x86_64-") {
        &["x64", "x86"]
    } else {
        &["x86", "x64", "arm64"]
    };
    let mut sdk_roots = Vec::new();
    if let Some(sdk_dir) = env::var_os("WindowsSdkDir") {
        sdk_roots.push(PathBuf::from(sdk_dir));
    }
    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(program_files) = env::var_os(variable) {
            sdk_roots.push(
                PathBuf::from(&program_files)
                    .join("Windows Kits")
                    .join("10"),
            );
            sdk_roots.push(
                PathBuf::from(program_files)
                    .join("Windows Kits")
                    .join("8.1"),
            );
        }
    }
    for sdk_root in sdk_roots {
        if let Some(compiler) = compiler_in_sdk(&sdk_root, architectures) {
            return compiler;
        }
    }
    if let Some(compiler) = resolve_executable("rc.exe") {
        return compiler;
    }
    panic!("Windows SDK rc.exe was not found; install the Windows SDK or set RC to its full path");
}

fn compiler_in_sdk(sdk_root: &Path, architectures: &[&str]) -> Option<PathBuf> {
    let bin_dir = sdk_root.join("bin");
    let mut versions = Vec::new();
    if let Ok(entries) = fs::read_dir(&bin_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(version) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let numbers: Option<Vec<u32>> =
                version.split('.').map(|part| part.parse().ok()).collect();
            if let Some(numbers) = numbers {
                versions.push((numbers, path));
            }
        }
    }
    versions.sort_by(|left, right| right.0.cmp(&left.0));
    let mut directories: Vec<PathBuf> = versions.into_iter().map(|(_, path)| path).collect();
    if let Ok(version) = env::var("WindowsSDKVersion") {
        let version = version.trim().trim_end_matches(['\\', '/']);
        if !version.is_empty() {
            directories.insert(0, bin_dir.join(version));
        }
    }
    directories.push(bin_dir);
    for directory in directories {
        for architecture in architectures {
            let compiler = directory.join(architecture).join("rc.exe");
            if compiler.is_file() {
                return Some(compiler);
            }
        }
        let compiler = directory.join("rc.exe");
        if compiler.is_file() {
            return Some(compiler);
        }
    }
    None
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(name);
    if path.is_file() {
        return Some(path);
    }
    if path.components().count() != 1 {
        return None;
    }
    env::var_os("PATH").and_then(|search_path| {
        env::split_paths(&search_path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

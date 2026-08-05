use std::env;
use std::path::PathBuf;

mod build {
    use super::*;
    use std::path::Path;
    use std::process::{Command, Stdio};

    const REPO: &str = "https://code.videolan.org/videolan/dav1d.git";
    const TAG: &str = "1.5.0";

    /// Locate vcvars64.bat in the standard Visual Studio install locations.
    fn find_vcvars() -> Option<PathBuf> {
        let drive = env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let bases = [
            format!("{}\\Program Files (x86)\\Microsoft Visual Studio", drive),
            format!("{}\\Program Files\\Microsoft Visual Studio", drive),
        ];
        let years = ["2022", "2019", "2017"];
        let editions = ["BuildTools", "Community", "Professional", "Enterprise"];
        let rel = Path::new("VC").join("Auxiliary").join("Build").join("vcvars64.bat");
        for base in bases.iter() {
            for year in years.iter() {
                for edition in editions.iter() {
                    let p = Path::new(base).join(year).join(edition).join(&rel);
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    /// Directories containing helper tools (nasm, pkg-config, meson, ninja).
    /// Override with the DAV1D_TOOLS_DIR env var (;-separated).
    fn tool_dirs() -> Vec<String> {
        if let Ok(d) = env::var("DAV1D_TOOLS_DIR") {
            return d.split(';').filter(|s| !s.is_empty()).map(str::to_string).collect();
        }
        // Defaults for this machine (paths that are NOT reachable from the
        // repo's own drive-mapped bash; the physical layout is D:\c\Tools).
        vec![
            "D:\\c\\Tools\\nasm\\nasm-3.02".to_string(),
            "D:\\c\\Tools\\pkgconfig\\pkg-config-lite-0.28-1\\bin".to_string(),
            "D:\\Works\\Python\\Scripts".to_string(),
        ]
    }

    /// Prepend helper tool dirs to PATH so meson/ninja/pkg-config/nasm are found.
    pub fn ensure_tool_path() {
        let extra = tool_dirs();
        let mut path = env::var("PATH").unwrap_or_default();
        for d in extra.iter().rev() {
            path = format!("{d};{path}");
        }
        env::set_var("PATH", path);
    }

    /// Run a plain command (no MSVC env) and fail on non-zero exit.
    fn run_plain(cmd: &str, args: &[String]) {
        let result = Command::new(cmd)
            .args(args)
            .stderr(Stdio::inherit())
            .stdout(Stdio::inherit())
            .output()
            .expect("failed");
        if !result.status.success() {
            panic!("{cmd} exited with {}", result.status);
        }
    }

    /// Run a command inside the MSVC developer environment so that cl.exe and
    /// link.exe (not MinGW) are selected by meson. Uses a temporary batch file
    /// to avoid cmd nested-quoting issues with paths that contain spaces.
    fn run_in_vs_env(cmd: &str, args: &[String]) {
        let Some(vcvars) = find_vcvars() else {
            run_plain(cmd, args);
            return;
        };

        let mut line = format!("@echo off\r\ncall \"{}\"\r\nif errorlevel 1 exit /b 1\r\n", vcvars.display().to_string());
        line.push_str(&cmd.to_string());
        for a in args {
            line.push(' ');
            line.push('"');
            line.push_str(a);
            line.push('"');
        }
        line.push_str("\r\nif errorlevel 1 exit /b 1\r\n");

        let bat = std::env::temp_dir().join(format!("dav1d-build-{}", std::process::id())).with_extension("bat");
        std::fs::write(&bat, line).expect("write temp bat");
        let result = Command::new("cmd")
            .arg("/c")
            .arg(&bat)
            .stderr(Stdio::inherit())
            .stdout(Stdio::inherit())
            .output()
            .expect("cmd failed");
        let _ = std::fs::remove_file(&bat);
        if !result.status.success() {
            panic!("{cmd} exited with {}", result.status);
        }
    }

    pub fn build_from_src(
        lib: &str,
        _version: &str,
    ) -> Result<system_deps::Library, system_deps::BuildInternalClosureError> {
        ensure_tool_path();

        let source = PathBuf::from(env::var("OUT_DIR").unwrap()).join("dav1d");
        let build_path = source.join("build");
        let release_path = source.join("release");

        // Remove any stale configuration (a previous run may have configured
        // with a different compiler), so meson always configures fresh with MSVC.
        let _ = std::fs::remove_dir_all(&build_path);
        let _ = std::fs::remove_dir_all(&release_path);

        if !Path::new(&source.join(".git")).exists() {
            run_plain(
                "git",
                &["clone", "--depth", "1", "-b", TAG, REPO, source.to_str().unwrap()]
                    .map(str::to_string),
            );
        } else {
            run_plain(
                "git",
                &[
                    "-C",
                    source.to_str().unwrap(),
                    "fetch",
                    "--depth",
                    "1",
                    "origin",
                    TAG,
                ]
                .map(str::to_string),
            );
            run_plain("git", &["-C", source.to_str().unwrap(), "checkout", "FETCH_HEAD"].map(str::to_string));
        }

        run_in_vs_env(
            "meson",
            &[
                "setup".to_string(),
                "-Ddefault_library=static".to_string(),
                // The bundled tools/tests are not needed by the Rust bindings and
                // require a resource compiler (windres), which breaks on Windows
                // when the build path contains spaces (MinGW bug #4933).
                "-Denable_tools=false".to_string(),
                "-Denable_tests=false".to_string(),
                "--prefix".to_string(),
                release_path.to_str().unwrap().to_string(),
                build_path.to_str().unwrap().to_string(),
                source.to_str().unwrap().to_string(),
            ],
        );
        run_in_vs_env("ninja", &["-C".to_string(), build_path.to_str().unwrap().to_string()]);
        run_in_vs_env("meson", &["install".to_string(), "-C".to_string(), build_path.to_str().unwrap().to_string()]);

        let pkg_dir = build_path.join("meson-private");
        system_deps::Library::from_internal_pkg_config(pkg_dir, lib, TAG)
    }
}

fn main() {
    if std::env::var("DOCS_RS").is_ok() {
        return;
    }

    build::ensure_tool_path();

    // Always build dav1d from source via the internal build closure instead of
    // probing for a system-installed library (there is none on Windows).
    std::env::set_var("SYSTEM_DEPS_DAV1D_BUILD_INTERNAL", "always");

    system_deps::Config::new()
        .add_build_internal("dav1d", build::build_from_src)
        .probe()
        .unwrap();
}

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(bypass_io_native_spdk)");
    println!("cargo:rustc-check-cfg=cfg(bypass_io_native_dpdk)");
    println!("cargo:rerun-if-env-changed=BYPASS_IO_NATIVE_SPDK");
    println!("cargo:rerun-if-env-changed=SPDK_USE_PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=SPDK_PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=SPDK_PKG_CONFIG_LIBS");
    println!("cargo:rerun-if-env-changed=SPDK_LIB_DIR");
    println!("cargo:rerun-if-env-changed=SPDK_LIBS");
    println!("cargo:rerun-if-env-changed=SPDK_SYSTEM_LIBS");
    println!("cargo:rerun-if-env-changed=SPDK_LINK_KIND");
    println!("cargo:rerun-if-env-changed=SPDK_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=BYPASS_IO_NATIVE_DPDK");
    println!("cargo:rerun-if-env-changed=DPDK_PKG_CONFIG_NAME");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_LIBDIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_SYSROOT_DIR");

    if feature_enabled("SPDK") && env_enabled("BYPASS_IO_NATIVE_SPDK") {
        link_spdk();
    }

    if feature_enabled("DPDK") && env_enabled("BYPASS_IO_NATIVE_DPDK") {
        link_dpdk();
    }
}

fn feature_enabled(name: &str) -> bool {
    env::var_os(format!("CARGO_FEATURE_{name}")).is_some()
}

fn env_enabled(name: &str) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set when native linking is enabled"))
}

fn split_csv_env(name: &str, default: &[&str]) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .filter(|values: &Vec<String>| !values.is_empty())
        .unwrap_or_else(|| default.iter().map(|value| (*value).to_owned()).collect())
}

fn link_spdk() {
    if env_enabled("SPDK_USE_PKG_CONFIG") {
        link_spdk_with_pkg_config();
        compile_native_shim(
            "bypass_spdk_shim",
            "native/bypass_spdk_shim.c",
            &spdk_cflags(),
        );
        return;
    }

    let spdk_lib_dir = required_env("SPDK_LIB_DIR");
    let spdk_include_dir = required_env("SPDK_INCLUDE_DIR");
    let link_kind = env::var("SPDK_LINK_KIND").unwrap_or_else(|_| "static".to_owned());
    let spdk_libs = split_csv_env("SPDK_LIBS", &["spdk_nvme", "spdk_env_dpdk", "spdk_util"]);
    let system_libs = split_csv_env("SPDK_SYSTEM_LIBS", &["numa", "dl", "pthread", "rt"]);

    compile_native_shim(
        "bypass_spdk_shim",
        "native/bypass_spdk_shim.c",
        &[format!("-I{spdk_include_dir}")],
    );
    println!("cargo:rustc-link-search=native={spdk_lib_dir}");
    for lib in spdk_libs {
        println!("cargo:rustc-link-lib={link_kind}={lib}");
    }
    for lib in system_libs {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rustc-cfg=bypass_io_native_spdk");
}

fn link_spdk_with_pkg_config() {
    let packages = split_csv_env("SPDK_PKG_CONFIG_LIBS", &["spdk_nvme", "spdk_env_dpdk"]);
    emit_pkg_config_libs(&packages, false);
    emit_pkg_config_libs(&["spdk_syslibs".to_owned()], true);
    println!("cargo:rustc-cfg=bypass_io_native_spdk");
}

fn link_dpdk() {
    let package = env::var("DPDK_PKG_CONFIG_NAME").unwrap_or_else(|_| "libdpdk".to_owned());
    let cflags = pkg_config_output(std::slice::from_ref(&package), &["--cflags"], false);
    let cflags = cflags
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    compile_native_shim("bypass_dpdk_shim", "native/bypass_dpdk_shim.c", &cflags);
    emit_pkg_config_libs(&[package], false);
    println!("cargo:rustc-cfg=bypass_io_native_dpdk");
}

fn spdk_cflags() -> Vec<String> {
    let packages = split_csv_env("SPDK_PKG_CONFIG_LIBS", &["spdk_nvme", "spdk_env_dpdk"]);
    pkg_config_output(&packages, &["--cflags"], false)
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

fn emit_pkg_config_libs(packages: &[String], include_static: bool) {
    let libs = pkg_config_output(packages, &["--libs"], include_static);
    for token in libs.split_whitespace() {
        emit_pkg_config_link_token(token);
    }
}

fn pkg_config_output(packages: &[String], args: &[&str], include_static: bool) -> String {
    let pkg_config = env::var_os("PKG_CONFIG").unwrap_or_else(|| OsString::from("pkg-config"));
    let mut command = Command::new(&pkg_config);
    command.args(args);
    if include_static {
        command.arg("--static");
    }
    command.args(packages);
    if let Ok(path) = env::var("SPDK_PKG_CONFIG_PATH") {
        command.env("PKG_CONFIG_PATH", path);
    }
    let output = command.output().unwrap_or_else(|error| {
        panic!(
            "failed to execute {} {:?} for {:?}: {error}",
            pkg_config.to_string_lossy(),
            args,
            packages
        )
    });

    if !output.status.success() {
        panic!(
            "{} {:?} {:?} failed: {}",
            pkg_config.to_string_lossy(),
            args,
            packages,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("pkg-config output for {packages:?} was not UTF-8: {error}"))
}

fn emit_pkg_config_link_token(token: &str) {
    if let Some(path) = token.strip_prefix("-L") {
        println!("cargo:rustc-link-search=native={path}");
    } else if let Some(lib) = token.strip_prefix("-l") {
        println!("cargo:rustc-link-lib={lib}");
    } else if token == "-pthread" || token.starts_with("-Wl,") {
        println!("cargo:rustc-link-arg={token}");
    }
}

fn compile_native_shim(name: &str, source: &str, cflags: &[String]) {
    let out_dir = PathBuf::from(required_env("OUT_DIR"));
    let object = out_dir.join(format!("{name}.o"));
    let archive = out_dir.join(format!("lib{name}.a"));
    let source = Path::new(source);

    let cc = env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let mut compile = Command::new(&cc);
    compile
        .arg("-std=c11")
        .arg("-fPIC")
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(&object);
    for flag in cflags {
        compile.arg(flag);
    }
    let output = compile.output().unwrap_or_else(|error| {
        panic!("failed to execute C compiler for {source:?}: {error}");
    });
    if !output.status.success() {
        panic!(
            "failed to compile {source:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let ar = env::var_os("AR").unwrap_or_else(|| OsString::from("ar"));
    let output = Command::new(&ar)
        .arg("crs")
        .arg(&archive)
        .arg(&object)
        .output()
        .unwrap_or_else(|error| panic!("failed to execute ar for {archive:?}: {error}"));
    if !output.status.success() {
        panic!(
            "failed to archive {archive:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static={name}");
    println!("cargo:rerun-if-changed={}", source.display());
}

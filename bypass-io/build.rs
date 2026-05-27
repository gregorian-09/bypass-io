use std::env;
use std::ffi::OsString;
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(bypass_io_native_spdk)");
    println!("cargo:rustc-check-cfg=cfg(bypass_io_native_dpdk)");
    println!("cargo:rerun-if-env-changed=BYPASS_IO_NATIVE_SPDK");
    println!("cargo:rerun-if-env-changed=SPDK_LIB_DIR");
    println!("cargo:rerun-if-env-changed=SPDK_LIBS");
    println!("cargo:rerun-if-env-changed=SPDK_SYSTEM_LIBS");
    println!("cargo:rerun-if-env-changed=SPDK_LINK_KIND");
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
    let spdk_lib_dir = required_env("SPDK_LIB_DIR");
    let link_kind = env::var("SPDK_LINK_KIND").unwrap_or_else(|_| "static".to_owned());
    let spdk_libs = split_csv_env("SPDK_LIBS", &["spdk_nvme", "spdk_env_dpdk", "spdk_util"]);
    let system_libs = split_csv_env("SPDK_SYSTEM_LIBS", &["numa", "dl", "pthread", "rt"]);

    println!("cargo:rustc-link-search=native={spdk_lib_dir}");
    for lib in spdk_libs {
        println!("cargo:rustc-link-lib={link_kind}={lib}");
    }
    for lib in system_libs {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rustc-cfg=bypass_io_native_spdk");
}

fn link_dpdk() {
    let pkg_config = env::var_os("PKG_CONFIG").unwrap_or_else(|| OsString::from("pkg-config"));
    let package = env::var("DPDK_PKG_CONFIG_NAME").unwrap_or_else(|_| "libdpdk".to_owned());
    let output = Command::new(&pkg_config)
        .arg("--libs")
        .arg(&package)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to execute {} for {package}: {error}",
                pkg_config.to_string_lossy()
            )
        });

    if !output.status.success() {
        panic!(
            "{} --libs {package} failed: {}",
            pkg_config.to_string_lossy(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let libs = String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("pkg-config output for {package} was not UTF-8: {error}"));
    for token in libs.split_whitespace() {
        emit_pkg_config_link_token(token);
    }
    println!("cargo:rustc-cfg=bypass_io_native_dpdk");
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

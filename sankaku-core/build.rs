use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=include/sankaku.h");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("out dir"));

    let header_src = manifest_dir.join("include").join("sankaku.h");
    let header_dst = out_dir.join("sankaku.h");
    if let Some(parent) = header_dst.parent() {
        fs::create_dir_all(parent).expect("create header output dir");
    }
    fs::copy(&header_src, &header_dst).expect("copy sankaku.h into OUT_DIR");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" {
        let def_path = out_dir.join("sankaku.exports.def");
        fs::write(
            &def_path,
            "LIBRARY sankaku\nEXPORTS\n    init\n    sankaku_stream_create\n    sankaku_stream_destroy\n    sankaku_stream_send_frame\n    sankaku_stream_poll_frame\n    sankaku_frame_free\n",
        )
        .expect("write sankaku export definition");

        if target_env == "msvc" {
            println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def_path.display());
        }
    }
}

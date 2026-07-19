//! Compile `proto/**` into prost types at build time, using protox (pure-Rust) as the parser so no
//! system `protoc` is required — the build stays hermetic and offline.

use std::path::PathBuf;

fn main() {
    let proto = "proto/imbh/v1/query.proto";
    println!("cargo:rerun-if-changed={proto}");
    println!("cargo:rerun-if-changed=proto");

    // protox parses the .proto (and its imports) into a FileDescriptorSet entirely in Rust.
    let fds = protox::compile([proto], ["proto"]).expect("protox: compile query.proto");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_fds(fds)
        .expect("prost-build: generate types from descriptor set");
}

// OUT_DIR=../../packages/proto/src/protos cargo run

use std::path::PathBuf;

fn main() {
    let includes = "../protospec";
    let out_dir = PathBuf::from("../../proto/src/protos");

    let ext = std::ffi::OsStr::new("proto");
    let protos: Vec<_> = walkdir::WalkDir::new(includes)
        .into_iter()
        .map(|x| x.unwrap())
        .filter(|x| x.path().extension() == Some(ext))
        .map(|x| x.into_path())
        .collect();

    println!("[info ] Compiling {} ...", includes);

    tonic_build::configure()
        .file_descriptor_set_path(out_dir.join("service_descriptor.bin"))
        .build_client(true)
        .compile_well_known_types(true)
        .client_mod_attribute(".", r#"#[cfg(feature = "client")]"#)
        .build_server(true)
        .server_mod_attribute(".", r#"#[cfg(feature = "server")]"#)
        .disable_comments("../protospec/google/protobuf/any.proto")
        .disable_comments("../protospec/google/api/http.proto")
        .out_dir(out_dir)
        .compile(&protos, &[includes])
        .unwrap();
}

use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Support both monorepo layout and standalone repo.
    let (proto_file, include_dir) =
        if Path::new("../../libs/proto/orchestra/plugin/v1/plugin.proto").exists() {
            // Inside framework monorepo.
            (
                "../../libs/proto/orchestra/plugin/v1/plugin.proto",
                "../../libs/proto",
            )
        } else {
            // Standalone repo — proto files bundled at proto/.
            ("proto/orchestra/plugin/v1/plugin.proto", "proto")
        };

    prost_build::Config::new()
        .compile_protos(&[proto_file], &[include_dir])?;
    Ok(())
}

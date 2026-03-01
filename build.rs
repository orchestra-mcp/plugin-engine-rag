fn main() -> Result<(), Box<dyn std::error::Error>> {
    prost_build::Config::new()
        .compile_protos(
            &["../../libs/proto/orchestra/plugin/v1/plugin.proto"],
            &["../../libs/proto"],
        )?;
    Ok(())
}

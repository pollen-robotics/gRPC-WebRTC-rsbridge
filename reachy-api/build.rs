fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("../deps/reachy2-sdk-api/protos/reachy.proto").unwrap();
    tonic_build::compile_protos("../deps/reachy2-sdk-api/protos/webrtc_bridge.proto").unwrap();
    Ok(())
}

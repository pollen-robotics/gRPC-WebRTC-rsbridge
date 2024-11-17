fn main() -> Result<(), Box<dyn std::error::Error>> {
    /*tonic_build::configure()
        .build_server(false)
        //.out_dir("src/google")  // you can change the generated code's location
        .compile_protos(
            &["deps/reachy2-sdk-api/protos/reachy.proto"], // specify the proto files to compile
            &["deps/reachy2-sdk-api/protos"], // specify the root location to search proto dependencies
        )
        .unwrap();

    tonic_build::configure()
        .build_server(false)
        //.out_dir("src/google")  // you can change the generated code's location
        .compile_protos(
            &["deps/reachy2-sdk-api/protos/webrtc_bridge.proto"], // specify the proto files to compile
            &["deps/reachy2-sdk-api/protos"], // specify the root location to search proto dependencies
        )
        .unwrap();*/

    tonic_build::compile_protos("deps/reachy2-sdk-api/protos/reachy.proto").unwrap();
    tonic_build::compile_protos("deps/reachy2-sdk-api/protos/webrtc_bridge.proto").unwrap();
    Ok(())
}

use clap::Parser;
use log::{debug, info};

mod webrtc;
use webrtc::webrtc_bridge::WebRTCBridge;

mod grpc;

//mod grpc_client;
//use grpc_client::GrpcClient;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Name as a producer
    #[arg(long, default_value = "grpc_webrtc_bridge")]
    producer_name: String,

    /// Gstreamer signalling server ip
    #[arg(long, default_value = "127.0.0.1")]
    signalling_host: String,

    /// Gstreamer signaling server port
    #[arg(long, default_value_t = 8443)]
    signalling_port: u16,
}

fn display_args(args: &Args) {
    info!("Args:");
    info!(
        "Signalling server: {}:{}",
        args.signalling_host, args.signalling_port
    );
    info!("Remote peer name: {:?}", args.producer_name);
}

fn main() {
    env_logger::init();
    info!("Starting grpc webrtc bridge");

    let args = Args::parse();

    display_args(&args);

    gst::init().unwrap();

    let uri = format!("ws://{}:{}", args.signalling_host, args.signalling_port);

    let server = WebRTCBridge::new(uri, args.producer_name);
    server.run();
    drop(server);

    //let grpc_client = GrpcClient::new("localhost", 50051);

    info!("exit server");

    unsafe { gst::deinit() };

    info!("Exiting grpc webrtc bridge");
}

use clap::Parser;
use log::{debug, info};
use signal_hook::{consts::SIGINT, iterator::Signals};
mod webrtc;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;
use webrtc::webrtc_bridge::WebRTCBridge;

mod grpc;
mod ros2;
use ros2::Ros2Publisher;

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

    /// SDK Server server ip
    #[arg(long, default_value = "127.0.0.1")]
    grpc_host: String,

    /// SDK Server server port
    #[arg(long, default_value_t = 50051)]
    grpc_port: u16,
}

fn display_args(args: &Args) {
    info!("Args:");
    info!(
        "Signalling server: {}:{}",
        args.signalling_host, args.signalling_port
    );
    info!("GRPC server: {}:{}", args.grpc_host, args.grpc_port);
    info!("Remote peer name: {:?}", args.producer_name);
}

fn main() {
    env_logger::init();

    info!("Starting grpc webrtc bridge");

    let args = Args::parse();

    display_args(&args);

    // Initialize ROS2 publisher
    info!("Initializing ROS2...");
    let ros2_publisher = Arc::new(
        Ros2Publisher::new().expect("Failed to create ROS2 publisher"),
    );
    info!("ROS2 initialized");

    gst::init().unwrap();

    let uri = format!("ws://{}:{}", args.signalling_host, args.signalling_port);
    let grpc_address = format!("http://{}:{}", args.grpc_host, args.grpc_port);

    let (tx_stop_signal, rx_stop_signal) = channel::<bool>();

    thread::spawn(move || {
        let mut signals = Signals::new([SIGINT]).unwrap();
        for sig in signals.forever() {
            debug!("Received SIGINT signal: {:?}", sig);
            let _ = tx_stop_signal.send(true);
        }
    });

    let server = WebRTCBridge::new(
        uri,
        args.producer_name,
        grpc_address,
        rx_stop_signal,
        ros2_publisher,
    );

    server.run();
    drop(server);

    info!("exit server");

    unsafe { gst::deinit() };

    info!("Exiting grpc webrtc bridge");
}

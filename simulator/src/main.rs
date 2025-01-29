//use crate::simulator::Simulator;
use clap::Parser;
use log::{debug, info, warn};
use signal_hook::{consts::SIGINT, iterator::Signals};
use std::sync::mpsc::channel;
use std::thread;

mod listener;
mod simulator;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Gstreamer signalling server ip
    #[arg(long, default_value = "127.0.0.1")]
    signalling_host: String,

    /// Gstreamer signaling server port
    #[arg(long, default_value_t = 8443)]
    signalling_port: u16,

    /// Name of the producer (i.e. bridge)
    #[arg(long, default_value = "grpc_webrtc_bridge")]
    producer_name: String,

    /// Sending frequency
    #[arg(long, default_value_t = 100)]
    frequency: u16,

    /// Perfomance benchmarking
    #[arg(long, default_value_t = false)]
    bench_mode: bool,
}

fn display_args(args: &Args) {
    info!("Args:");
    info!(
        "Signalling server: {}:{}",
        args.signalling_host, args.signalling_port
    );
    info!("producer name: {}", args.producer_name);
    info!(
        "Parameters: frequency {}, benchmarking {}",
        args.frequency, args.bench_mode
    );
}

fn main() {
    env_logger::init();

    info!("Starting teleoperation simulator");

    let args = Args::parse();

    display_args(&args);

    gst::init().unwrap();

    let uri = format!("ws://{}:{}", args.signalling_host, args.signalling_port);

    let listener = listener::Listener::new(&uri, args.producer_name);
    if let Some(peer_id) = listener.look_for_peer_id() {
        info!("Peer id found: {}", peer_id);

        let (tx_stop_signal, rx_stop_signal) = channel::<bool>();

        thread::spawn(move || {
            let mut signals = Signals::new([SIGINT]).unwrap();
            for sig in signals.forever() {
                debug!("Received SIGINT signal: {:?}", sig);
                let _ = tx_stop_signal.send(true);
            }
        });

        let simulator = simulator::Simulator::new(uri, peer_id, rx_stop_signal, args.frequency);
        simulator.run();
    } else {
        warn!("No peer id found!");
    }

    unsafe { gst::deinit() };

    info!("Exiting teleoperation simulator");
}

# Grpc <-> WebRTC bridge

This project is a bridge between a gRPC server and a WebRTC client. It expose the gRPC SDK server to a WebRTC client.

## Usage

### Requirements

A signalling server must be up and running.

### Server
```bash
RUST_LOG=debug cargo run --bin grpc-webrtc-rsbridge
```

### Client

The client sends messages to move both Reachy's arms. Optional arguments are `--frequency`, to
 set the base uploading frequency, and `--bench_mode` to increase the frequency until 1500Hz.

```bash
RUST_LOG=debug cargo run --bin simulator -- --frequency 100 --bench-mode
```

Recorded data (a.k.a. Gaelle Fantome) can be found in the `data/` folder.

```bash
RUST_LOG=debug cargo run --bin simulator -- --frequency 120 --recorded-data
```
*Note : start the command from the root of the repository. The data path is hardcoded*

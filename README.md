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

The client sends messages to move both Reachy's arms.

```bash
RUST_LOG=debug cargo run --bin simulator
```

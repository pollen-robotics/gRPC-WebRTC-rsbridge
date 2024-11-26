use log::debug;

use crate::grpc::reachy_api::reachy::reachy_service_client::ReachyServiceClient;
use crate::grpc::reachy_api::reachy::Reachy;
use crate::grpc::reachy_api::reachy::ReachyStreamAuditRequest;
use crate::grpc::reachy_api::reachy::ReachyStreamStateRequest;
use tokio::runtime::Runtime;
use tonic::transport::Channel;

use super::reachy_api::reachy::ReachyId;
use super::reachy_api::reachy::ReachyState;
use super::reachy_api::reachy::ReachyStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct GrpcClient {
    client: ReachyServiceClient<Channel>,
    rt: Runtime, // see https://tokio.rs/tokio/topics/bridging
    stop_flag: Arc<AtomicBool>,
}

impl GrpcClient {
    pub fn new(address: String) -> Self {
        debug!("Constructor Grpc client {address}");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let client = rt
            .block_on(ReachyServiceClient::connect(address))
            .expect("Connection failed");

        Self {
            client: client,
            rt: rt,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    pub fn get_reachy(&mut self) -> Reachy {
        debug!("Get reachy");
        let request = tonic::Request::new(());

        self.rt
            .block_on(self.client.get_reachy(request))
            .unwrap()
            .into_inner()
    }

    pub fn get_reachy_state(
        &mut self,
        reachy: Option<ReachyId>,
        freq: f32,
        tx: std::sync::mpsc::Sender<ReachyState>,
    ) {
        let request = tonic::Request::new(ReachyStreamStateRequest {
            id: reachy,
            publish_frequency: freq,
        });

        let mut stream = self
            .rt
            .block_on(self.client.stream_reachy_state(request))
            .unwrap()
            .into_inner();

        let stop_flag = self.stop_flag.clone();

        self.rt.spawn(async move {
            while let Some(state) = stream.message().await.unwrap() {
                let _ = tx.send(state);
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
            }
            drop(tx);
        });
        //info!("exit get state");
    }

    pub fn get_reachy_audit_status(
        &mut self,
        reachy: Option<ReachyId>,
        freq: f32,
        tx: std::sync::mpsc::Sender<ReachyStatus>,
    ) {
        let request = tonic::Request::new(ReachyStreamAuditRequest {
            id: reachy,
            publish_frequency: freq,
        });

        let mut stream = self
            .rt
            .block_on(self.client.stream_audit(request))
            .unwrap()
            .into_inner();

        let stop_flag = self.stop_flag.clone();

        self.rt.spawn(async move {
            while let Some(status) = stream.message().await.unwrap() {
                let _ = tx.send(status);
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
            }
            drop(tx);
        });
    }
}

#[test]
fn test_get_reachy() {
    env_logger::init();

    let grpc_address = format!("http://localhost:50051");

    let mut grpc_client = GrpcClient::new(grpc_address);

    let reachy: Reachy = grpc_client.get_reachy();
    let reachy_id = reachy.id.unwrap();

    assert!(
        !reachy_id.name.is_empty(),
        "Reachy name should not be empty"
    );
    assert!(reachy_id.id > 0, "Reachy id should be greater than 0");
}

use log::{debug, warn};

use crate::grpc::reachy_api::reachy;
use crate::grpc::reachy_api::reachy::part::arm;
use crate::grpc::reachy_api::reachy::part::arm::arm_service_client::ArmServiceClient;
use crate::grpc::reachy_api::reachy::part::head::head_service_client::HeadServiceClient;
use crate::grpc::reachy_api::reachy::part::hand::hand_service_client::HandServiceClient;
use crate::grpc::reachy_api::reachy::part::mobile::base::mobility::mobile_base_mobility_service_client::MobileBaseMobilityServiceClient;
use crate::grpc::reachy_api::reachy::part::mobile::base::utility::mobile_base_utility_service_client::MobileBaseUtilityServiceClient;
use crate::grpc::reachy_api::reachy::reachy_service_client::ReachyServiceClient;
use crate::grpc::reachy_api::reachy::Reachy;
use crate::grpc::reachy_api::reachy::ReachyStreamAuditRequest;
use crate::grpc::reachy_api::reachy::ReachyStreamStateRequest;
use tokio::runtime::Runtime;
use tonic::transport::Channel;

use super::reachy_api::reachy::ReachyId;
use super::reachy_api::reachy::ReachyState;
use super::reachy_api::reachy::ReachyStatus;
use crate::grpc::reachy_api::bridge::{
    any_command, AnyCommands, ArmCommand, HandCommand, MobileBaseCommand, NeckCommand,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct GrpcClient {
    reachy_stub: ReachyServiceClient<Channel>,
    arm_stub: ArmServiceClient<Channel>,
    head_stub: HeadServiceClient<Channel>,
    hand_stub: HandServiceClient<Channel>,
    mobilebase_mobility_stub: MobileBaseMobilityServiceClient<Channel>,
    mobilebase_utility_stub: MobileBaseUtilityServiceClient<Channel>,
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

        let reachy_stub = rt
            .block_on(ReachyServiceClient::connect(address.clone()))
            .expect("Connection failed");

        let arm_stub = rt
            .block_on(ArmServiceClient::connect(address.clone()))
            .expect("Connection failed");

        let hand_stub = rt
            .block_on(HandServiceClient::connect(address.clone()))
            .expect("Connection failed");

        let head_stub = rt
            .block_on(HeadServiceClient::connect(address.clone()))
            .expect("Connection failed");

        let mobilebase_mobility_stub = rt
            .block_on(MobileBaseMobilityServiceClient::connect(address.clone()))
            .expect("Connection failed");

        let mobilebase_utility_stub = rt
            .block_on(MobileBaseUtilityServiceClient::connect(address))
            .expect("Connection failed");

        Self {
            reachy_stub,
            arm_stub,
            head_stub,
            hand_stub,
            mobilebase_mobility_stub,
            mobilebase_utility_stub,
            rt,
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
            .block_on(self.reachy_stub.get_reachy(request))
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
            .block_on(self.reachy_stub.stream_reachy_state(request))
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
            .block_on(self.reachy_stub.stream_audit(request))
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

    pub fn handle_commands(&mut self, commands: AnyCommands) {
        debug!("Received message: {:?}", commands);

        for cmd in commands.commands {
            match cmd.command {
                Some(any_command::Command::ArmCommand(arm_cmd)) => self.handle_arm_command(arm_cmd),
                Some(any_command::Command::HandCommand(hand_cmd)) => {
                    self.handle_hand_command(hand_cmd)
                }
                Some(any_command::Command::NeckCommand(neck_cmd)) => {
                    self.handle_neck_command(neck_cmd)
                }
                Some(any_command::Command::MobileBaseCommand(mobile_base_cmd)) => {
                    self.handle_mobile_base_command(mobile_base_cmd)
                }
                _ => warn!("Unknown command: {:?}", cmd),
            }
        }
    }

    fn handle_arm_command(&mut self, cmd: ArmCommand) {
        debug!("Handling arm command: {:?}", cmd);

        if let Some(arm_cartesian_goal) = cmd.arm_cartesian_goal {
            debug!("SendArmCartesianGoal");
            self.rt
                .block_on(self.arm_stub.send_arm_cartesian_goal(arm_cartesian_goal))
                .unwrap();
        } else if let Some(turn_on) = cmd.turn_on {
            debug!("arm_turn_on");
            self.rt.block_on(self.arm_stub.turn_on(turn_on)).unwrap();
        } else if let Some(turn_off) = cmd.turn_off {
            debug!("arm_turn_off");
            self.rt.block_on(self.arm_stub.turn_off(turn_off)).unwrap();
        } else if let Some(speed_limit) = cmd.speed_limit {
            debug!("arm_speed_limit");
            self.rt
                .block_on(self.arm_stub.set_speed_limit(speed_limit))
                .unwrap();
        } else if let Some(torque_limit) = cmd.torque_limit {
            debug!("arm_torque_limit");
            self.rt
                .block_on(self.arm_stub.set_torque_limit(torque_limit))
                .unwrap();
        } else {
            debug!("Unknown arm command: {:?}", cmd);
        }
    }
    fn handle_hand_command(&mut self, cmd: HandCommand) {
        debug!("Handling hand command: {:?}", cmd);

        if let Some(hand_goal) = cmd.hand_goal {
            debug!("hand_goal");
            self.rt
                .block_on(self.hand_stub.set_hand_position(hand_goal))
                .unwrap();
        } else if let Some(turn_on) = cmd.turn_on {
            debug!("hand_turn_on");
            self.rt.block_on(self.hand_stub.turn_on(turn_on)).unwrap();
        } else if let Some(turn_off) = cmd.turn_off {
            debug!("hand_turn_off");
            self.rt.block_on(self.hand_stub.turn_off(turn_off)).unwrap();
        } else {
            debug!("Unknown hand command: {:?}", cmd);
        }
    }

    fn handle_neck_command(&mut self, cmd: NeckCommand) {
        debug!("Handling neck command: {:?}", cmd);

        if let Some(neck_goal) = cmd.neck_goal {
            debug!("neck_goal");
            self.rt
                .block_on(self.head_stub.send_neck_joint_goal(neck_goal))
                .unwrap();
        } else if let Some(turn_on) = cmd.turn_on {
            debug!("neck_turn_on");
            self.rt.block_on(self.head_stub.turn_on(turn_on)).unwrap();
        } else if let Some(turn_off) = cmd.turn_off {
            debug!("neck_turn_off");
            self.rt.block_on(self.head_stub.turn_off(turn_off)).unwrap();
        } else if let Some(speed_limit) = cmd.speed_limit {
            debug!("neck_speed_limit");
            self.rt
                .block_on(self.head_stub.set_speed_limit(speed_limit))
                .unwrap();
        } else if let Some(torque_limit) = cmd.torque_limit {
            debug!("neck_torque_limit");
            self.rt
                .block_on(self.head_stub.set_torque_limit(torque_limit))
                .unwrap();
        } else {
            debug!("Unknown neck command: {:?}", cmd);
        }
    }

    fn handle_mobile_base_command(&mut self, cmd: MobileBaseCommand) {
        debug!("Handling mobile base command: {:?}", cmd);

        if let Some(target_direction) = cmd.target_direction {
            debug!("mobile_base_target_direction");
            self.rt
                .block_on(
                    self.mobilebase_mobility_stub
                        .send_direction(target_direction),
                )
                .unwrap();
        } else if let Some(mobile_base_mode) = cmd.mobile_base_mode {
            debug!("mobile_base_mode");
            self.rt
                .block_on(self.mobilebase_utility_stub.set_zuuu_mode(mobile_base_mode))
                .unwrap();
        } else {
            debug!("Unknown mobile base command: {:?}", cmd);
        }
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

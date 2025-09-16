use gst::glib;
use gst::glib::WeakRef;
use gst::prelude::*;
use gstrswebrtc::signaller::Signallable;
use gstrswebrtc::signaller::SignallableExt;
use gstrswebrtc::signaller::Signaller;
use gstrswebrtc::signaller::WebRTCSignallerRole;
use gstwebrtc::WebRTCDataChannel;
use log::{debug, error, info, trace, warn};
use prost::Message;
use reachy_api::bridge::any_command::Command::{
    AntennasCommand, ArmCommand, HandCommand, MobileBaseCommand, NeckCommand,
};
use reachy_api::bridge::service_response::Response;
use reachy_api::bridge::{service_request, Connect, GetReachy, ServiceRequest, ServiceResponse};
use reachy_api::bridge::{AnyCommand, AnyCommands};
use reachy_api::component::ComponentId;
use reachy_api::reachy;
use reachy_api::reachy::kinematics::rotation3d::Rotation;
use reachy_api::reachy::kinematics::Matrix4x4;
use reachy_api::reachy::kinematics::Quaternion;
use reachy_api::reachy::kinematics::Rotation3d;
use reachy_api::reachy::part::arm::{ArmCartesianGoal, SpeedLimitRequest as ArmSpeedLimitRequest, TorqueLimitRequest as ArmTorqueLimitRequest};
use reachy_api::reachy::part::hand::parallel_gripper_position::GripperPosition;
use reachy_api::reachy::part::hand::HandPosition;
use reachy_api::reachy::part::hand::HandPositionRequest;
use reachy_api::reachy::part::head::{NeckJointGoal, SpeedLimitRequest as NeckSpeedLimitRequest, TorqueLimitRequest as NeckTorqueLimitRequest};
use reachy_api::reachy::part::head::NeckOrientation;
use reachy_api::reachy::part::mobile::base::mobility::DirectionVector;
use reachy_api::reachy::part::mobile::base::utility::ZuuuModeCommand;
use reachy_api::reachy::part::PartId;
use reachy_api::reachy::{Reachy, ReachyState, ReachyStatus};
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;
use std::{fs, io};

pub struct Simulator {
    signaller: Signaller,
    pipeline: gst::Pipeline,
    main_loop: Arc<glib::MainLoop>,
    _reachy: Arc<Mutex<Option<Reachy>>>, //keeps a ref alive because of weak ref in configure_data_channels.
}

impl Simulator {
    pub fn new(
        uri: String,
        peer_id: String,
        rx_stop_signal: std::sync::mpsc::Receiver<bool>,
        frequency: u16,
        bench_mode: bool,
        recorded_data: bool,
        test: bool,
    ) -> Self {
        let main_loop = Arc::new(glib::MainLoop::new(None, false));
        let main_loop_clone = main_loop.clone();

        std::thread::spawn(move || {
            let _ = rx_stop_signal.recv();
            main_loop_clone.quit();
        });

        let signaller = Signaller::new(WebRTCSignallerRole::Consumer);
        signaller.set_property("uri", &uri);
        signaller.set_property("producer-peer-id", &peer_id);

        let _reachy: Arc<Mutex<Option<Reachy>>> = Arc::new(Mutex::new(None));
        let (pipeline, webrtcbin) = Simulator::setup_webrtc(
            &peer_id,
            _reachy.clone(),
            main_loop.clone(),
            frequency,
            bench_mode,
            recorded_data,
            test,
        );

        signaller.connect_closure(
            "error",
            false,
            glib::closure!(
                #[strong]
                main_loop,
                move |_signaler: glib::Object, error: String| {
                    error!("Signalling error: {}. Shutting down", error);
                    main_loop.quit();
                }
            ),
        );

        signaller.connect_closure(
            "session-ended",
            false,
            glib::closure!(
                #[strong]
                main_loop,
                move |_signaler: glib::Object, session_id: &str| {
                    info!("session-ended signal {}", session_id);
                    main_loop.quit();
                    false
                }
            ),
        );

        signaller.connect_closure(
            "session-started",
            false,
            glib::closure!(
                |_signaller: glib::Object, session_id: &str, peer_id: &str| {
                    debug! {"session started: session {} peer {}", session_id, peer_id};
                }
            ),
        );

        signaller.connect_closure(
            "session-description",
            false,
            glib::closure!(
                move |signaler: glib::Object,
                      session_id: &str,
                      session_description: &gstwebrtc::WebRTCSessionDescription| {
                    //let signaler_arc =
                    //    Arc::new(Mutex::new(signaler.downcast::<Signallable>().unwrap()));
                    //let signaler_arc_clone = signaler_arc.clone();
                    let signaler_ref = signaler.downcast::<Signallable>().unwrap().downgrade();
                    let webrtcbin_ref = webrtcbin.downgrade();

                    if session_description.type_() == gstwebrtc::WebRTCSDPType::Offer {
                        info!("Got offer for session {}", session_id);
                        //let signaler_ref_clone = signaler_ref.clone();
                        Simulator::connect_webrtcbin_to_ice(
                            webrtcbin_ref.clone(),
                            signaler_ref.clone(),
                            session_id.to_string(),
                        );

                        webrtcbin.emit_by_name::<()>(
                            "set-remote-description",
                            &[session_description, &None::<gst::Promise>],
                        );
                        Simulator::create_answer(
                            webrtcbin_ref,
                            signaler_ref,
                            session_id.to_string(),
                        );
                    } else {
                        error!("Unsupported SDP Type");
                    }
                }
            ),
        );

        Self {
            signaller,
            pipeline,
            main_loop,
            _reachy,
        }
    }

    fn setup_webrtc(
        peer_id: &String,
        reachy: Arc<Mutex<Option<Reachy>>>,
        main_loop: Arc<glib::MainLoop>,
        frequency: u16,
        bench_mode: bool,
        recorded_data: bool,
        test: bool,
    ) -> (gst::Pipeline, gst::Element) {
        let pipeline = gst::Pipeline::builder()
            .name(format!("session-pipeline-{peer_id}"))
            .build();

        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .build()
            .expect("Failed to create webrtcbin");

        pipeline.add(&webrtcbin).unwrap();

        //let webrtcbin_arc = Arc::new(Mutex::new(webrtcbin));
        let webrtcbin_ref = webrtcbin.downgrade();

        let ret = pipeline.set_state(gst::State::Playing);
        match ret {
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::Async) => {
                // Pipeline state changed successfully
                Simulator::configure_data_channels(
                    webrtcbin_ref,
                    reachy,
                    main_loop,
                    frequency,
                    bench_mode,
                    recorded_data,
                    test,
                );
            }
            Ok(gst::StateChangeSuccess::NoPreroll) => {
                error!("Failed to transition pipeline to PLAYING: No preroll data available");
            }
            Err(err) => {
                error!("Failed to transition pipeline to PLAYING: {:?}", err);
            }
        }
        (pipeline, webrtcbin)
    }

    fn configure_data_channels(
        webrtcbin: WeakRef<gst::Element>,
        reachy: Arc<Mutex<Option<Reachy>>>,
        main_loop: Arc<glib::MainLoop>,
        frequency: u16,
        bench_mode: bool,
        recorded_data: bool,
        test: bool,
    ) {
        webrtcbin.upgrade().unwrap().connect_closure(
            "on-data-channel",
            false,
            glib::closure!(
                #[weak]
                reachy,
                #[weak]
                main_loop,
                move |_webrtcbin: &gst::Element, channel: &WebRTCDataChannel| {
                    let label = channel.property::<String>("label");

                    if label == "service" {
                        debug!("Received service data channel");
                        Simulator::configure_service_channel(channel, reachy);
                        Simulator::start_connection(channel);
                    } else if label.starts_with("reachy_state") {
                        debug!("Received reachy state data channel");
                        Simulator::configure_reachy_state_channel(channel);
                    } else if label.starts_with("reachy_audit") {
                        debug!("Received reachy audit data channel");
                        Simulator::configure_reachy_audit_channel(channel);
                    } else if label.starts_with("reachy_command_lossy") {
                        debug!("Received reachy command lossy data channel");
                        Simulator::send_commands(
                            channel.clone(),
                            reachy,
                            main_loop,
                            frequency,
                            bench_mode,
                            recorded_data,
                            test,
                        );
                    } else if label.starts_with("reachy_command_reliable") {
                        debug!("Received reachy command reliable data channel");
                        Simulator::speed_limit_command("l_arm", 10, channel);
                        Simulator::speed_limit_command("r_arm", 10, channel);
                        Simulator::speed_limit_command("head", 100, channel);
                        Simulator::turn_on_robot(channel, reachy);
                        Simulator::torque_limit_command("l_arm", 100, channel);
                        Simulator::torque_limit_command("r_arm", 100, channel);
                        Simulator::torque_limit_command("head", 100, channel);

                    } else {
                        warn!("Received unknown data channel: {}", label);
                    }
                }
            ),
        );
    }

    fn send_commands(
        channel: WebRTCDataChannel,
        reachy: Arc<Mutex<Option<Reachy>>>,
        main_loop: Arc<glib::MainLoop>,
        frequency: u16,
        bench_mode: bool,
        recorded_data: bool,
        test: bool,
    ) {
        let main_loop_clone = main_loop.clone();
        let frequency = Arc::new(AtomicU64::new(frequency as u64));
        let frequency_clone = frequency.clone();
        if bench_mode {
            std::thread::spawn(move || {
                while main_loop_clone.is_running() {
                    std::thread::sleep(Duration::from_millis(50));

                    let mut frequency_local = frequency_clone.load(Ordering::Relaxed);
                    frequency_local += 1;
                    let sample_duration = Duration::from_micros(1000000 / frequency_local);
                    debug!(
                        "frequency: {frequency_local} sample duration {}",
                        sample_duration.as_micros()
                    );
                    if frequency_local > 1500 {
                        main_loop_clone.quit();
                    } else {
                        frequency_clone.store(frequency_local, Ordering::Relaxed);
                    }
                }
            });
        }

        if recorded_data {
            let results = Simulator::open_txt_files_in_data();
            /*debug!(
                "Number of txt files found in 'data': {}",
                results.unwrap().len()
            );*/
            if let Ok(files) = results {
                std::thread::spawn(move || {
                    if files.len() == 0 {
                        error!("Data files are empty");
                        return;
                    }
                    debug!("{} files found", files.len());

                    let lines_vecs: Vec<Vec<&str>> = files
                        .iter()
                        .map(|(_, content)| content.lines().collect())
                        .collect();

                    let lens: Vec<usize> = lines_vecs.iter().map(|lines| lines.len()).collect();
                    let nb_files = lines_vecs.len();
                    let mut idxs = vec![0usize; nb_files];

                    while main_loop.is_running() {
                        let mut commands = Vec::new();
                        for (f, lines) in lines_vecs.iter().enumerate() {
                            if !lines.is_empty() {
                                let i = idxs[f];
                                if let Some(cmd) = Simulator::any_command_from_line(lines[i]) {
                                    commands.push(cmd);
                                }
                            }
                        }

                        let data = glib::Bytes::from_owned(
                            AnyCommands { commands: commands }.encode_to_vec(),
                        );
                        channel.send_data(Some(&data));

                        for (i, idx) in idxs.iter_mut().enumerate() {
                            *idx += 1;
                            if *idx >= lens[i] {
                                *idx = 0;
                            }
                        }

                        let sample_duration =
                            Duration::from_micros(1000000 / frequency.load(Ordering::Relaxed));
                        std::thread::sleep(sample_duration);
                    }
                });
            }
        } else if test {
            let t0 = std::time::Instant::now();
            let duration = 3.0;
            let elevation_delta = 0.05;
            Simulator::spawn_gotoposture_motion_thread(
                main_loop.clone(),
                t0,
                duration,
                elevation_delta,
                reachy.clone(),
                channel.clone(),
                frequency.clone(),
            );

            std::thread::sleep(Duration::from_secs(3));
            Simulator::speed_limit_command("l_arm", 100, &channel);
            Simulator::speed_limit_command("r_arm", 100, &channel);

        } else {
            let radius = 0.1f64; //Circle radius
            let fixed_x = 0.4f64; // Fixed x-coordinate
            let center_y = 0f64;
            let center_z = -0.1f64; // Center of the circle in y-z plane
            let circle_period = 3f64;
            let t0 = Instant::now();
            Simulator::spawn_circle_motion_thread(
                main_loop.clone(),
                t0,
                circle_period,
                center_y,
                center_z,
                radius,
                fixed_x,
                reachy,
                channel,
                frequency.clone(),
            );
        }
    }

    fn speed_limit_command(part_name: &str, limit: u32, channel: &WebRTCDataChannel) -> () {
        let id = match part_name {
            "r_arm" => 1,
            "l_arm" => 2,
            "head" => 3,
            _ => {
                error!("Unknown part name: {}", part_name);
                return;
            }
        };

        let command = if id == 3 {
            AnyCommand {
                command: Some(NeckCommand(reachy_api::bridge::NeckCommand {
                    speed_limit: Some(NeckSpeedLimitRequest {
                        id: Some(PartId {
                            id: id,
                            name: part_name.to_string(),
                        }),
                        limit: limit,
                    }),
                    ..Default::default()
                })),
            }
        } else {
            AnyCommand {
                command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                    speed_limit: Some(ArmSpeedLimitRequest {
                        id: Some(PartId {
                            id: id,
                            name: part_name.to_string(),
                        }),
                        limit: limit,
                    }),
                    ..Default::default()
                })),
            }
        };

        let commands = AnyCommands {
            commands: Vec::from([command]),
        };

        let data = glib::Bytes::from_owned(commands.encode_to_vec());
        channel.send_data(Some(&data));
    }

    fn torque_limit_command(part_name: &str, limit: u32, channel: &WebRTCDataChannel) -> () {
        let id = match part_name {
            "r_arm" => 1,
            "l_arm" => 2,
            "head" => 3,
            _ => {
                error!("Unknown part name: {}", part_name);
                return;
            }
        };

        let command = if id == 3 {
            AnyCommand {
                command: Some(NeckCommand(reachy_api::bridge::NeckCommand {
                    torque_limit: Some(NeckTorqueLimitRequest {
                        id: Some(PartId {
                            id: id,
                            name: part_name.to_string(),
                        }),
                        limit: limit,
                    }),
                    ..Default::default()
                })),
            }
        } else {
            AnyCommand {
                command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                    torque_limit: Some(ArmTorqueLimitRequest {
                        id: Some(PartId {
                            id: id,
                            name: part_name.to_string(),
                        }),
                        limit: limit,
                    }),
                    ..Default::default()
                })),
            }
        };

        let commands = AnyCommands {
            commands: Vec::from([command]),
        };

        let data = glib::Bytes::from_owned(commands.encode_to_vec());
        channel.send_data(Some(&data));
    }

    fn create_cartesian_goal_command(arm_name: &str, matrix: Vec<f64>) -> AnyCommand {
        let id = match arm_name {
        "l_arm" => 2,
        "r_arm" => 1,
        _ => panic!("Unknown arm name: {}", arm_name),
        };
        AnyCommand {
            command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                arm_cartesian_goal: Some(ArmCartesianGoal {
                    id: Some(PartId {
                        id,
                        name: arm_name.to_string(),
                    }),
                    goal_pose: Some(Matrix4x4 { data: matrix }),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        }
    }

    fn spawn_gotoposture_motion_thread(
        main_loop: Arc<glib::MainLoop>,
        t0: std::time::Instant,
        period: f64,
        elevation_delta: f64,
        reachy: Arc<Mutex<Option<Reachy>>>,
        channel: WebRTCDataChannel,
        frequency: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) {
        // Goto posture : default
        let l_arm_matrix_base = vec![
            1.0, -0.001, -0.015, 0.022,
           -0.001, 0.996, -0.086, 0.252,
           0.015, 0.086, 0.996, -0.608,
           0.0, 0.0, 0.0, 1.0,
        ];
        let r_arm_matrix_base = vec![
            1.0, 0.001, -0.015, 0.022,
            0.001, 0.996, 0.086, -0.252,
            0.015, -0.086, 0.996, -0.608,
            0.0, 0.0, 0.0, 1.0,
        ];

        let z_1 = l_arm_matrix_base[11];
        let z_2 = z_1 + elevation_delta;

        std::thread::spawn(move || {
            let mut forward = true;
            let mut current_t0 = t0;
            let sample_duration = Duration::from_micros(1_000_000 / frequency.load(Ordering::Relaxed));

            while main_loop.is_running() {
                let start_time = std::time::Instant::now();
                let elapsed_time = current_t0.elapsed();
                let t = (elapsed_time.as_secs_f64() % period) / period;

                let z_l = if forward {
                    z_1 + (z_2 - z_1) * t
                } else {
                    z_2 + (z_1 - z_2) * t
                };
                let z_r = z_l;

                let mut l_arm_matrix = l_arm_matrix_base.clone();
                l_arm_matrix[11] = z_l;
                let mut r_arm_matrix = r_arm_matrix_base.clone();
                r_arm_matrix[11] = z_r;

                let left_arm = Simulator::create_cartesian_goal_command("l_arm", l_arm_matrix);
                let right_arm = Simulator::create_cartesian_goal_command("r_arm", r_arm_matrix);

                let commands = AnyCommands {
                    commands: vec![left_arm, right_arm],
                };
                let data = glib::Bytes::from_owned(commands.encode_to_vec());
                channel.send_data(Some(&data));

                let loop_time = start_time.elapsed();
                if loop_time < sample_duration {
                    std::thread::sleep(sample_duration - loop_time);
                }

                if elapsed_time.as_secs_f64() >= period {
                    forward = !forward;
                    current_t0 = std::time::Instant::now();
                }
            }

            Simulator::torque_limit_command("l_arm", 35, &channel);
            Simulator::torque_limit_command("r_arm", 35, &channel);
            Simulator::speed_limit_command("l_arm", 10, &channel);
            Simulator::speed_limit_command("r_arm", 10, &channel);
            Simulator::torque_limit_command("l_arm", 25, &channel);
            Simulator::torque_limit_command("r_arm", 25, &channel);
            Simulator::speed_limit_command("l_arm", 15, &channel);
            Simulator::speed_limit_command("r_arm", 15, &channel);
            Simulator::speed_limit_command("l_arm", 5, &channel);
            Simulator::speed_limit_command("r_arm", 5, &channel);
            Simulator::turn_off_robot(&channel, reachy.clone());
        });
    }

    fn any_command_from_line(line: &str) -> Option<AnyCommand> {
        let v: Value = serde_json::from_str(line).ok()?;
        let arr = v.as_array()?;
        let obj = arr.get(0)?.as_object()?;

        if let Some(val) = obj.get("armCommand") {
            return Some(AnyCommand {
                command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                    arm_cartesian_goal: match val.get("armCartesianGoal") {
                        None => None,
                        Some(acg_val) => Some(ArmCartesianGoal {
                            id: match acg_val.get("id") {
                                None => None,
                                Some(id_obj) => Some(PartId {
                                    id: id_obj.get("id").and_then(|id| id.as_u64()).unwrap_or(0)
                                        as u32,
                                    name: id_obj
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                }),
                            },
                            //duration: Some(1.0),
                            goal_pose: match acg_val
                                .get("goalPose")
                                .and_then(|gp| gp.get("data"))
                                .and_then(|goal_pose_data| goal_pose_data.as_array())
                            {
                                None => None,
                                Some(arr) => Some(Matrix4x4 {
                                    data: arr.iter().filter_map(|v| v.as_f64()).collect(),
                                }),
                            },
                            ..Default::default()
                        }),
                    },
                    speed_limit: match val.get("speedLimit") {
                        None => None,
                        Some(sl_val) => Some(ArmSpeedLimitRequest {
                            id: match sl_val.get("id") {
                                None => None,
                                Some(id_obj) => Some(PartId {
                                    id: id_obj.get("id").and_then(|id| id.as_u64()).unwrap_or(0)
                                        as u32,
                                    name: id_obj
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                }),
                            },
                            limit: sl_val.get("limit").and_then(|l| l.as_u64()).unwrap_or(0) as u32,
                        }),
                    },
                    ..Default::default()
                })),
            });
        }

        if let Some(val) = obj.get("neckCommand") {
            debug!("neckCommand found {}", val);
            return Some(AnyCommand {
                command: Some(NeckCommand(reachy_api::bridge::NeckCommand {
                    neck_goal: Some(NeckJointGoal {
                        id: Some(PartId {
                            id: val
                                .get("neckGoal")
                                .and_then(|nc| nc.get("id"))
                                .unwrap()
                                .get("id")
                                .and_then(|id| id.as_u64())
                                .unwrap() as u32,

                            name: val
                                .get("neckGoal")
                                .and_then(|nc| nc.get("id"))
                                .unwrap()
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap()
                                .to_string(),
                        }),
                        joints_goal: Some(NeckOrientation {
                            rotation: Some(Rotation3d {
                                rotation: Some(Rotation::Q(Quaternion {
                                    w: val
                                        .get("neckGoal")
                                        .and_then(|nc| nc.get("jointsGoal"))
                                        .and_then(|o| o.get("rotation"))
                                        .and_then(|r| r.get("q"))
                                        .and_then(|q| q.get("w"))
                                        .and_then(|w| w.as_f64())
                                        .unwrap(),
                                    x: val
                                        .get("neckGoal")
                                        .and_then(|nc| nc.get("jointsGoal"))
                                        .and_then(|o| o.get("rotation"))
                                        .and_then(|r| r.get("q"))
                                        .and_then(|q| q.get("x"))
                                        .and_then(|x| x.as_f64())
                                        .unwrap(),
                                    y: val
                                        .get("neckGoal")
                                        .and_then(|nc| nc.get("jointsGoal"))
                                        .and_then(|o| o.get("rotation"))
                                        .and_then(|r| r.get("q"))
                                        .and_then(|q| q.get("y"))
                                        .and_then(|y| y.as_f64())
                                        .unwrap(),
                                    z: val
                                        .get("neckGoal")
                                        .and_then(|nc| nc.get("jointsGoal"))
                                        .and_then(|o| o.get("rotation"))
                                        .and_then(|r| r.get("q"))
                                        .and_then(|q| q.get("z"))
                                        .and_then(|z| z.as_f64())
                                        .unwrap(),
                                })),
                            }),
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(val) = obj.get("antennasCommand") {
            debug!("antennasCommand found {}", val);
            return Some(AnyCommand {
                command: Some(AntennasCommand(
                    reachy_api::component::dynamixel_motor::DynamixelMotorsCommand {
                        cmd: Vec::from([
                            reachy_api::component::dynamixel_motor::DynamixelMotorCommand {
                                id: Some(ComponentId {
                                    name: "antenna_left".to_string(),
                                    ..Default::default()
                                }),
                                goal_position: val
                                    .get("cmd")
                                    .and_then(|cmd| cmd.get(0))
                                    .and_then(|item| item.get("goalPosition"))
                                    .and_then(|gp| gp.as_f64())
                                    .map(|p| p as f32),
                                ..Default::default()
                            },
                            reachy_api::component::dynamixel_motor::DynamixelMotorCommand {
                                id: Some(ComponentId {
                                    name: "antenna_right".to_string(),
                                    ..Default::default()
                                }),
                                goal_position: val
                                    .get("cmd")
                                    .and_then(|cmd| cmd.get(1))
                                    .and_then(|item| item.get("goalPosition"))
                                    .and_then(|gp| gp.as_f64())
                                    .map(|p| p as f32),
                                ..Default::default()
                            },
                        ]),
                    },
                )),
            });
        }

        if let Some(val) = obj.get("handCommand") {
            debug!("handCommand found {}", val);
            return Some(AnyCommand {
                command: Some(HandCommand(reachy_api::bridge::HandCommand {
                    hand_goal: Some(HandPositionRequest {
                        id: Some(PartId {
                            id: val
                                .get("handGoal")
                                .and_then(|hc| hc.get("id"))
                                .unwrap()
                                .get("id")
                                .and_then(|id| id.as_u64())
                                .unwrap() as u32,

                            name: val
                                .get("handGoal")
                                .and_then(|hg| hg.get("id"))
                                .unwrap()
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap()
                                .to_string(),
                        }),
                        position: Some(HandPosition {
                            position: Some(
                                reachy::part::hand::hand_position::Position::ParallelGripper(
                                    reachy::part::hand::ParallelGripperPosition {
                                        gripper_position: Some(GripperPosition::OpeningPercentage(
                                            val.get("handGoal")
                                                .and_then(|hc| hc.get("position"))
                                                .and_then(|hc| hc.get("parallelGripper"))
                                                .and_then(|hg| hg.get("openingPercentage"))
                                                .and_then(|p| p.as_f64())
                                                .unwrap_or(0.0) //sometimes it's empty
                                                as f32,
                                        )),
                                    },
                                ),
                            ),
                        }),
                    }),

                    ..Default::default()
                })),
            });
        }

        if let Some(val) = obj.get("mobileBaseCommand") {
            debug!("mobileBaseCommand found {}", val);
            return Some(AnyCommand {
                command: Some(MobileBaseCommand(reachy_api::bridge::MobileBaseCommand {
                    mobile_base_mode: match val.get("mobileBaseMode") {
                        None => None,
                        Some(m) if m == "CMD_VEL" => Some(ZuuuModeCommand {
                            mode: 1,
                            ..Default::default()
                        }),
                        _ => return None,
                    },
                    target_direction: match val.get("targetDirection") {
                        None => return None,
                        Some(target_direction_val) => Some(
                            reachy::part::mobile::base::mobility::TargetDirectionCommand {
                                id: Some(PartId {
                                    id: target_direction_val
                                        .get("id")
                                        .unwrap()
                                        .get("id")
                                        .and_then(|id| id.as_u64())
                                        .unwrap() as u32,

                                    name: target_direction_val
                                        .get("id")
                                        .unwrap()
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap()
                                        .to_string(),
                                }),
                                direction: Some(DirectionVector {
                                    x: Some(
                                        target_direction_val
                                            .get("direction")
                                            .and_then(|d| d.get("x"))
                                            .and_then(|x| x.as_f64())
                                            .unwrap()
                                            as f32,
                                    ),
                                    y: Some(
                                        target_direction_val
                                            .get("direction")
                                            .and_then(|d| d.get("y"))
                                            .and_then(|y| y.as_f64())
                                            .unwrap()
                                            as f32,
                                    ),
                                    theta: Some(
                                        target_direction_val
                                            .get("direction")
                                            .and_then(|d| d.get("theta"))
                                            .and_then(|z| z.as_f64())
                                            .unwrap()
                                            as f32,
                                    ),
                                }),
                            },
                        ),
                    },
                })),
            });
        }
        None
    }

    fn spawn_circle_motion_thread(
        main_loop: Arc<glib::MainLoop>,
        t0: std::time::Instant,
        circle_period: f64,
        center_y: f64,
        center_z: f64,
        radius: f64,
        fixed_x: f64,
        reachy: Arc<Mutex<Option<Reachy>>>,
        channel: WebRTCDataChannel,
        frequency: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) {
        std::thread::spawn(move || {
            while main_loop.is_running() {
                let elapsed_time = t0.elapsed();
                let angle =
                    2f64 * std::f64::consts::PI * elapsed_time.as_secs_f64() / circle_period;

                let y = center_y + radius * angle.cos();
                let z = center_z + radius * angle.sin();

                let left_arm = AnyCommand {
                    command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                        arm_cartesian_goal: Some(ArmCartesianGoal {
                            id: reachy
                                .lock()
                                .unwrap()
                                .as_ref()
                                .unwrap()
                                .l_arm
                                .as_ref()
                                .unwrap()
                                .part_id
                                .clone(),
                            duration: Some(1.0f32),
                            goal_pose: Some(Matrix4x4 {
                                data: Vec::from([
                                    0f64,
                                    0f64,
                                    -1f64,
                                    fixed_x,
                                    0f64,
                                    1f64,
                                    0f64,
                                    y + 0.2f64,
                                    1f64,
                                    0f64,
                                    0f64,
                                    z,
                                    0f64,
                                    0f64,
                                    0f64,
                                    1f64,
                                ]),
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })),
                };
                let right_arm = AnyCommand {
                    command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                        arm_cartesian_goal: Some(ArmCartesianGoal {
                            id: reachy
                                .lock()
                                .unwrap()
                                .as_ref()
                                .unwrap()
                                .r_arm
                                .as_ref()
                                .unwrap()
                                .part_id
                                .clone(),
                            duration: Some(1.0f32),
                            goal_pose: Some(Matrix4x4 {
                                data: Vec::from([
                                    0f64,
                                    0f64,
                                    -1f64,
                                    fixed_x,
                                    0f64,
                                    1f64,
                                    0f64,
                                    y - 0.2f64,
                                    1f64,
                                    0f64,
                                    0f64,
                                    z,
                                    0f64,
                                    0f64,
                                    0f64,
                                    1f64,
                                ]),
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })),
                };
                let commands = AnyCommands {
                    commands: Vec::from([left_arm, right_arm]),
                };
                let data = glib::Bytes::from_owned(commands.encode_to_vec());
                channel.send_data(Some(&data));
                let sample_duration =
                    Duration::from_micros(1000000 / frequency.load(Ordering::Relaxed));
                std::thread::sleep(sample_duration);
            }
        });
    }

    fn open_txt_files_in_data() -> io::Result<Vec<(String, String)>> {
        let mut results = Vec::new();
        let data_path = Path::new("simulator/data");

        if data_path.is_dir() {
            for entry in fs::read_dir(data_path)? {
                let entry = entry?;
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext == "txt" {
                        let file_name = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .to_string();
                        let mut file = fs::File::open(&path)?;
                        let mut contents = String::new();
                        file.read_to_string(&mut contents)?;
                        results.push((file_name, contents));
                    }
                }
            }
        }

        Ok(results)
    }

    fn turn_on_robot(channel: &WebRTCDataChannel, reachy: Arc<Mutex<Option<Reachy>>>) {
        if reachy.lock().unwrap().is_none() {
            warn!("cannot turn on. Reachy config not received");
            return;
        }

        let left_arm = AnyCommand {
            command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                turn_on: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .l_arm
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let left_hand = AnyCommand {
            command: Some(HandCommand(reachy_api::bridge::HandCommand {
                turn_on: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .l_hand
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let right_arm = AnyCommand {
            command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                turn_on: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .r_arm
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let right_hand = AnyCommand {
            command: Some(HandCommand(reachy_api::bridge::HandCommand {
                turn_on: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .r_hand
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let neck = AnyCommand {
            command: Some(NeckCommand(reachy_api::bridge::NeckCommand {
                turn_on: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .head
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };
        let mobilebase = AnyCommand {
            command: Some(MobileBaseCommand(reachy_api::bridge::MobileBaseCommand {
                mobile_base_mode: Some(ZuuuModeCommand {
                    mode: 1, //CMD_VEL
                    id: reachy
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .mobile_base
                        .as_ref()
                        .unwrap()
                        .part_id
                        .clone(),
                }),
                ..Default::default()
            })),
        };

        for command in [&left_arm, &right_arm,  &neck,  &left_hand, &right_hand, &mobilebase] {
            let commands = AnyCommands {
                commands: Vec::from([command.clone()]),
            };
            let data = glib::Bytes::from_owned(commands.encode_to_vec());
            channel.send_data(Some(&data));
        }

    }

    fn turn_off_robot(channel: &WebRTCDataChannel, reachy: Arc<Mutex<Option<Reachy>>>) {
        if reachy.lock().unwrap().is_none() {
            warn!("cannot turn off. Reachy config not received");
            return;
        }
        info!("turning off robot");

        let left_arm = AnyCommand {
            command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                turn_off: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .l_arm
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let left_hand = AnyCommand {
            command: Some(HandCommand(reachy_api::bridge::HandCommand {
                turn_off: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .l_hand
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let right_arm = AnyCommand {
            command: Some(ArmCommand(reachy_api::bridge::ArmCommand {
                turn_off: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .r_arm
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let right_hand = AnyCommand {
            command: Some(HandCommand(reachy_api::bridge::HandCommand {
                turn_off: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .r_hand
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };

        let neck = AnyCommand {
            command: Some(NeckCommand(reachy_api::bridge::NeckCommand {
                turn_off: reachy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .head
                    .as_ref()
                    .unwrap()
                    .part_id
                    .clone(),
                ..Default::default()
            })),
        };
        let mobilebase = AnyCommand {
            command: Some(MobileBaseCommand(reachy_api::bridge::MobileBaseCommand {
                mobile_base_mode: Some(ZuuuModeCommand {
                    mode: 3, //FREE_WHEEL
                    id: reachy
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .mobile_base
                        .as_ref()
                        .unwrap()
                        .part_id
                        .clone(),
                }),
                ..Default::default()
            })),
        };
        
        for command in [&left_arm, &right_arm,  &neck,  &left_hand, &right_hand, &mobilebase] {
            let commands = AnyCommands {
                commands: Vec::from([command.clone()]),
            };
            let data = glib::Bytes::from_owned(commands.encode_to_vec());
            channel.send_data(Some(&data));
        }
    }

    fn configure_reachy_state_channel(channel: &WebRTCDataChannel) {
        channel.connect_on_message_data(move |_: &WebRTCDataChannel, msg: Option<&glib::Bytes>| {
            let Some(data) = msg else {
                warn!("state message is None");
                return;
            };
            let state: ReachyState = match Message::decode(data.as_ref()) {
                Ok(state) => state,
                Err(e) => {
                    error!("Failed to decode message: {}", e);
                    return;
                }
            };

            trace!("Received state: {:?}", state);
        });
    }

    fn configure_reachy_audit_channel(channel: &WebRTCDataChannel) {
        channel.connect_on_message_data(move |_: &WebRTCDataChannel, msg: Option<&glib::Bytes>| {
            let Some(data) = msg else {
                warn!("audit message is None");
                return;
            };
            let status: ReachyStatus = match Message::decode(data.as_ref()) {
                Ok(status) => status,
                Err(e) => {
                    error!("Failed to decode message: {}", e);
                    return;
                }
            };

            trace!("Received status: {:?}", status);
        });
    }

    fn start_connection(channel: &WebRTCDataChannel) {
        let service_request = ServiceRequest {
            request: Some(service_request::Request::GetReachy(GetReachy {})),
        };
        let data = glib::Bytes::from_owned(service_request.encode_to_vec());
        channel.send_data(Some(&data));
    }

    fn configure_service_channel(channel: &WebRTCDataChannel, reachy: Arc<Mutex<Option<Reachy>>>) {
        channel.connect_on_message_data(
            move |channel: &WebRTCDataChannel, msg: Option<&glib::Bytes>| {
                let Some(data) = msg else {
                    warn!("service message is None");
                    return;
                };
                let response: ServiceResponse = match Message::decode(data.as_ref()) {
                    Ok(response) => response,
                    Err(e) => {
                        error!("Failed to decode message: {}", e);
                        return;
                    }
                };

                match response.response {
                    Some(Response::ConnectionStatus(connection_status)) => {
                        //info!("Connection status: {:?}", connection_status);
                        reachy
                            .lock()
                            .unwrap()
                            .replace(connection_status.reachy.unwrap());
                        let service_request = ServiceRequest {
                            request: Some(service_request::Request::Connect(Connect {
                                reachy_id: reachy.lock().unwrap().as_ref().unwrap().id.clone(),
                                update_frequency: 60f32,
                                audit_frequency: 1f32,
                            })),
                        };
                        let data = glib::Bytes::from_owned(service_request.encode_to_vec());
                        channel.send_data(Some(&data));
                    }
                    Some(Response::Error(error)) => {
                        error!("Received error message: {:?}", error);
                    }
                    None => {
                        error!("No response");
                    }
                }
            },
        );
    }

    fn connect_webrtcbin_to_ice(
        webrtcbin: WeakRef<gst::Element>,
        signaller: WeakRef<Signallable>,
        session_id: String,
    ) {
        let webrtcbin = webrtcbin.upgrade().unwrap();
        webrtcbin.connect_closure(
            "on-ice-candidate",
            false,
            glib::closure!(
                #[strong]
                session_id,
                move |_webrtcbin: &gst::Element, sdp_m_line_index: u32, candidate: String| {
                    debug!("adding ice candidate {} {} ", sdp_m_line_index, candidate);
                    signaller.upgrade().unwrap().add_ice(
                        &session_id,
                        &candidate,
                        sdp_m_line_index,
                        None,
                    )
                }
            ),
        );

        webrtcbin.connect_notify(
            Some("connection-state"),
            glib::clone!(
                #[strong]
                session_id,
                move |webrtcbin, _pspec| {
                    let state = webrtcbin
                        .property::<gstwebrtc::WebRTCPeerConnectionState>("connection-state");

                    match state {
                        gstwebrtc::WebRTCPeerConnectionState::Failed => {
                            warn!("Connection state for in session {} failed", session_id);
                        }
                        _ => {
                            info!(
                                "Connection state in session {}  changed: {:?}",
                                session_id, state
                            );
                        }
                    }
                }
            ),
        );

        webrtcbin.connect_notify(
            Some("ice-connection-state"),
            glib::clone!(
                #[strong]
                session_id,
                move |webrtcbin, _pspec| {
                    let state = webrtcbin
                        .property::<gstwebrtc::WebRTCICEConnectionState>("ice-connection-state");

                    match state {
                        gstwebrtc::WebRTCICEConnectionState::Failed => {
                            error!("Ice connection state in session {} failed", session_id);
                        }
                        _ => {
                            debug!(
                                "Ice connection state in session {} changed: {:?}",
                                session_id, state
                            );
                        }
                    }

                    if state == gstwebrtc::WebRTCICEConnectionState::Completed {
                        debug!("Ice connection state in session {} completed", session_id);
                    }
                }
            ),
        );
    }

    fn create_answer(
        webrtcbin: WeakRef<gst::Element>,
        signaller: WeakRef<Signallable>,
        session_id: String,
    ) {
        debug!("Creating answer for session");

        let webrtcbin_clone = webrtcbin.clone();

        let promise = gst::Promise::with_change_func(glib::clone!(move |reply| {
            let reply = match reply {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    debug!("Promise returned without a reply for");
                    return;
                }
                Err(err) => {
                    debug!("Promise returned with an error for: {:?}", err);
                    return;
                }
            };

            if let Ok(answer) = reply
                .value("answer")
                .map(|answer| answer.get::<gstwebrtc::WebRTCSessionDescription>().unwrap())
            {
                Simulator::on_answer_created(webrtcbin_clone, answer, signaller, session_id);
            } else {
                debug!("Reply without an answer for session: {:?}", reply);
            }
        }));
        webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
    }

    fn on_answer_created(
        webrtcbin: WeakRef<gst::Element>,
        answer: gstwebrtc::WebRTCSessionDescription,
        signaller_ref: WeakRef<Signallable>,
        session_id: String,
    ) {
        debug!("Set local description");
        webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);

        let signaller = signaller_ref.upgrade().unwrap();

        let maybe_munged_answer = if signaller
            .has_property("manual-sdp-munging", Some(bool::static_type()))
            && signaller.property("manual-sdp-munging")
        {
            // Don't munge, signaller will manage this
            answer
        } else {
            // Use the default munging mechanism (signal registered by user)
            signaller.munge_sdp(&session_id, &answer)
        };
        signaller.send_sdp(&session_id, &maybe_munged_answer);
    }

    pub fn run(&self) {
        self.signaller.start();
        info!("Simulator started");
        self.main_loop.run();
        info!("Simulator stopped");
        self.signaller.stop();
    }

    fn stop(&self) {
        debug!("stop simulator");
        let ret = self.pipeline.set_state(gst::State::Null);
        match ret {
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::Async) => {
                // Pipeline state changed successfully
            }
            Ok(gst::StateChangeSuccess::NoPreroll) => {
                error!("Failed to transition pipeline to NULL: No preroll data available");
            }
            Err(err) => {
                error!("Failed to transition pipeline to NULL: {:?}", err);
            }
        }
    }
}

impl Drop for Simulator {
    fn drop(&mut self) {
        self.stop();
    }
}

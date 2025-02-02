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
use reachy_api::bridge::any_command::Command::ArmCommand;
use reachy_api::bridge::service_response::Response;
use reachy_api::bridge::{service_request, Connect, GetReachy, ServiceRequest, ServiceResponse};
use reachy_api::bridge::{AnyCommand, AnyCommands};
use reachy_api::reachy::kinematics::Matrix4x4;
use reachy_api::reachy::part::arm::ArmCartesianGoal;
use reachy_api::reachy::{Reachy, ReachyState, ReachyStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

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
                        );
                    } else if label.starts_with("reachy_command_reliable") {
                        debug!("Received reachy command reliable data channel");
                        Simulator::turn_on_arms(channel, reachy);
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
    ) {
        let radius = 0.2f64; //Circle radius
        let fixed_x = 0.4f64; // Fixed x-coordinate
        let center_y = 0f64;
        let center_z = 0.1f64; // Center of the circle in y-z plane
                               //let mut frequency = frequency as u64; //Update frequency in Hz
        let frequency = Arc::new(AtomicU64::new(frequency as u64));
        let frequency_clone = frequency.clone();
        //let mut sample_duration = Duration::from_millis(1000 / frequency);
        let circle_period = 3f64;
        let t0 = Instant::now();

        let main_loop_clone = main_loop.clone();
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
                                    1f64,
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
                                    1f64,
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

    fn turn_on_arms(channel: &WebRTCDataChannel, reachy: Arc<Mutex<Option<Reachy>>>) {
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
        let commands = AnyCommands {
            commands: Vec::from([left_arm, right_arm]),
        };
        let data = glib::Bytes::from_owned(commands.encode_to_vec());
        channel.send_data(Some(&data));
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

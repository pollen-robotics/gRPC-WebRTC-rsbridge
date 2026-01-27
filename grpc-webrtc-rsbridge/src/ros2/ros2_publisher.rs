use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::thread;

/// Commands sent to the ROS2 publishing thread
#[derive(Debug, Clone)]
pub enum Ros2Command {
    PublishTorque { part: String, enabled: bool },
    PublishTorqueLimit { part: String, value: f64 },
    PublishSpeedLimit { part: String, value: f64 },
}

/// Joint mappings for torque enable/disable (actuator-level names)
const TORQUE_JOINTS: &[(&str, &[&str])] = &[
    ("r_arm", &["r_shoulder", "r_elbow", "r_wrist"]),
    ("l_arm", &["l_shoulder", "l_elbow", "l_wrist"]),
    ("neck", &["neck"]),
    ("head", &["neck"]), // alias for neck
    ("r_hand", &["r_hand"]),
    ("l_hand", &["l_hand"]),
    ("antenna_left", &["antenna_left"]),
    ("antenna_right", &["antenna_right"]),
];

/// Joint mappings for torque_limit and speed_limit (raw motor names)
const LIMIT_JOINTS: &[(&str, &[&str])] = &[
    (
        "r_arm",
        &[
            "r_shoulder_raw_motor_1",
            "r_shoulder_raw_motor_2",
            "r_elbow_raw_motor_1",
            "r_elbow_raw_motor_2",
            "r_wrist_raw_motor_1",
            "r_wrist_raw_motor_2",
            "r_wrist_raw_motor_3",
        ],
    ),
    (
        "l_arm",
        &[
            "l_shoulder_raw_motor_1",
            "l_shoulder_raw_motor_2",
            "l_elbow_raw_motor_1",
            "l_elbow_raw_motor_2",
            "l_wrist_raw_motor_1",
            "l_wrist_raw_motor_2",
            "l_wrist_raw_motor_3",
        ],
    ),
    (
        "neck",
        &["neck_raw_motor_1", "neck_raw_motor_2", "neck_raw_motor_3"],
    ),
    (
        "head", // alias for neck
        &["neck_raw_motor_1", "neck_raw_motor_2", "neck_raw_motor_3"],
    ),
    ("r_hand", &["r_hand_raw_motor_1"]),
    ("l_hand", &["l_hand_raw_motor_1"]),
    ("antenna_left", &["antenna_left_raw_motor"]),
    ("antenna_right", &["antenna_right_raw_motor"]),
];

/// Thread-safe wrapper that sends commands to the ROS2 thread
pub struct Ros2Publisher {
    tx: Sender<Ros2Command>,
}

impl Ros2Publisher {
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (tx, rx) = channel::<Ros2Command>();

        // Spawn dedicated ROS2 thread
        thread::spawn(move || {
            // Initialize ROS2 on this thread
            let ctx = match r2r::Context::create() {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to create ROS2 context: {:?}", e);
                    return;
                }
            };

            let mut node = match r2r::Node::create(ctx, "webrtc_bridge_publisher", "") {
                Ok(n) => n,
                Err(e) => {
                    error!("Failed to create ROS2 node: {:?}", e);
                    return;
                }
            };

            // RELIABLE QoS for at-least-once delivery
            let qos = r2r::QosProfile::default().reliable().keep_last(10);

            let publisher = match node
                .create_publisher::<r2r::control_msgs::msg::DynamicJointState>(
                    "/dynamic_joint_commands",
                    qos,
                ) {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to create ROS2 publisher: {:?}", e);
                    return;
                }
            };

            // Build lookup maps
            let torque_map: HashMap<String, Vec<String>> = TORQUE_JOINTS
                .iter()
                .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
                .collect();

            let limit_map: HashMap<String, Vec<String>> = LIMIT_JOINTS
                .iter()
                .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
                .collect();

            info!("ROS2 publisher thread started on /dynamic_joint_commands");

            // Process commands
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    Ros2Command::PublishTorque { part, enabled } => {
                        if let Some(joints) = torque_map.get(&part) {
                            let value = if enabled { 1.0 } else { 0.0 };
                            let msg = build_message(joints, "torque", value);
                            info!(
                                "Publishing torque {} for part {} - joints: {:?}",
                                if enabled { "ON" } else { "OFF" },
                                part,
                                joints
                            );
                            if let Err(e) = publisher.publish(&msg) {
                                error!("Failed to publish torque command: {:?}", e);
                            }
                            // Arms need extra time for the subscriber to process
                            if part == "l_arm" || part == "r_arm" {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                        } else {
                            warn!("Unknown part for torque: {}", part);
                        }
                    }
                    Ros2Command::PublishTorqueLimit { part, value } => {
                        if let Some(joints) = limit_map.get(&part) {
                            let msg = build_message(joints, "torque_limit", value);
                            debug!(
                                "Publishing torque_limit {} for part {} ({} joints)",
                                value,
                                part,
                                joints.len()
                            );
                            if let Err(e) = publisher.publish(&msg) {
                                error!("Failed to publish torque_limit command: {:?}", e);
                            }
                            // Arms need extra time for the subscriber to process
                            if part == "l_arm" || part == "r_arm" {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                        } else {
                            warn!("Unknown part for torque_limit: {}", part);
                        }
                    }
                    Ros2Command::PublishSpeedLimit { part, value } => {
                        if let Some(joints) = limit_map.get(&part) {
                            let msg = build_message(joints, "speed_limit", value);
                            debug!(
                                "Publishing speed_limit {} for part {} ({} joints)",
                                value,
                                part,
                                joints.len()
                            );
                            if let Err(e) = publisher.publish(&msg) {
                                error!("Failed to publish speed_limit command: {:?}", e);
                            }
                            // Arms need extra time for the subscriber to process
                            if part == "l_arm" || part == "r_arm" {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                        } else {
                            warn!("Unknown part for speed_limit: {}", part);
                        }
                    }
                }
                // Spin to ensure message delivery - longer delay for reliable QoS handshake
                for _ in 0..3 {
                    let _ = node.spin_once(std::time::Duration::from_millis(10));
                }
            }

            info!("ROS2 publisher thread exiting");
        });

        info!("ROS2 publisher initialized");
        Ok(Self { tx })
    }

    /// Publish torque enable (true) or disable (false) for a part
    pub fn publish_torque(&self, part: &str, enabled: bool) {
        if let Err(e) = self.tx.send(Ros2Command::PublishTorque {
            part: part.to_string(),
            enabled,
        }) {
            error!("Failed to send torque command to ROS2 thread: {:?}", e);
        }
    }

    /// Publish torque limit for a part (applied to all raw motors)
    pub fn publish_torque_limit(&self, part: &str, value: f64) {
        if let Err(e) = self.tx.send(Ros2Command::PublishTorqueLimit {
            part: part.to_string(),
            value,
        }) {
            error!("Failed to send torque_limit command to ROS2 thread: {:?}", e);
        }
    }

    /// Publish speed limit for a part (applied to all raw motors)
    pub fn publish_speed_limit(&self, part: &str, value: f64) {
        if let Err(e) = self.tx.send(Ros2Command::PublishSpeedLimit {
            part: part.to_string(),
            value,
        }) {
            error!("Failed to send speed_limit command to ROS2 thread: {:?}", e);
        }
    }
}

/// Build a DynamicJointState message
fn build_message(
    joints: &[String],
    interface: &str,
    value: f64,
) -> r2r::control_msgs::msg::DynamicJointState {
    use r2r::control_msgs::msg::{DynamicJointState, InterfaceValue};

    let joint_names: Vec<String> = joints.to_vec();

    // Each joint gets one InterfaceValue with the interface name and value
    let interface_values: Vec<InterfaceValue> = joints
        .iter()
        .map(|_| InterfaceValue {
            interface_names: vec![interface.to_string()],
            values: vec![value],
        })
        .collect();

    DynamicJointState {
        header: Default::default(),
        joint_names,
        interface_values,
    }
}

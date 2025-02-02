pub mod reachy {
    tonic::include_proto!("reachy");
    pub mod part {
        tonic::include_proto!("reachy.part");
        pub mod arm {
            tonic::include_proto!("reachy.part.arm");
        }
        pub mod hand {
            tonic::include_proto!("reachy.part.hand");
        }
        pub mod head {
            tonic::include_proto!("reachy.part.head");
        }
        pub mod mobile {
            pub mod base {
                pub mod mobility {
                    tonic::include_proto!("reachy.part.mobile.base.mobility");
                }
                pub mod utility {
                    tonic::include_proto!("reachy.part.mobile.base.utility");
                }
                pub mod lidar {
                    tonic::include_proto!("reachy.part.mobile.base.lidar");
                }
            }
        }
    }
    pub mod kinematics {
        tonic::include_proto!("reachy.kinematics");
    }
}

pub mod error {
    tonic::include_proto!("error");
}

pub mod component {
    tonic::include_proto!("component");
    pub mod orbita2d {
        tonic::include_proto!("component.orbita2d");
    }
    pub mod orbita3d {
        tonic::include_proto!("component.orbita3d");
    }
    pub mod dynamixel_motor {
        tonic::include_proto!("component.dynamixel_motor");
    }
    pub mod audio {
        tonic::include_proto!("component.audio");
    }
}

pub mod bridge {
    tonic::include_proto!("bridge");
}

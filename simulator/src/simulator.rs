use gst::glib;
use gst::prelude::*;
use gstrswebrtc::signaller::Signallable;
use gstrswebrtc::signaller::SignallableExt;
use gstrswebrtc::signaller::Signaller;
use gstrswebrtc::signaller::WebRTCSignallerRole;
use log::{debug, error, info, warn};
use std::sync::Arc;

pub struct Simulator {
    signaller: Signaller,
    main_loop: Arc<glib::MainLoop>,
}

impl Simulator {
    pub fn new(
        uri: String,
        peer_id: String,
        rx_stop_signal: std::sync::mpsc::Receiver<bool>,
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

        Self {
            signaller,
            main_loop,
        }
    }

    pub fn run(&self) {
        self.signaller.start();
        info!("Simulator started");
        self.main_loop.run();
        info!("Simulator stopped");
        self.signaller.stop();
    }
}

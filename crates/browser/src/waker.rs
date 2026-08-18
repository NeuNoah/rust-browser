//! Bridges Servo's event-loop wakeups to winit user events.
//!
//! Servo runs on background threads. Whenever it needs the main thread
//! to process messages, it calls [`EventLoopWaker::wake`], which posts
//! a [`AppEvent::Wake`] through the winit proxy. The main loop then
//! calls [`Servo::spin_event_loop`] to drain Servo's queues.

use winit::event_loop::EventLoopProxy;

use crate::app::AppEvent;

/// The winit-backed implementation of Servo's [`servo::EventLoopWaker`].
#[derive(Clone)]
pub struct EventLoopWaker {
    proxy: EventLoopProxy<AppEvent>,
}

impl EventLoopWaker {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        Self { proxy }
    }
}

impl servo::EventLoopWaker for EventLoopWaker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let _ = self.proxy.send_event(AppEvent::Wake);
    }
}

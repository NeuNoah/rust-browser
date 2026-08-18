//! Entry point: environment setup and the winit event loop.
//!
//! The TLS provider must be installed before the event loop starts so
//! that every network stack thread (Servo's included) shares the same
//! crypto provider.

#![forbid(unsafe_code)]

mod app;
mod clipboard;
mod gui;
mod waker;

use std::error::Error;

use app::{App, AppEvent};
use winit::event_loop::EventLoop;

fn main() -> Result<(), Box<dyn Error>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let initial_url = std::env::args().nth(1);
    let event_loop = EventLoop::<AppEvent>::with_user_event().build()?;
    let mut app = App::new(&event_loop, initial_url);
    event_loop.run_app(&mut app)?;
    Ok(())
}

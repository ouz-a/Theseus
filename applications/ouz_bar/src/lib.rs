#![no_std]

extern crate alloc;
#[macro_use]
extern crate log;
#[macro_use]
extern crate app_io;
extern crate async_channel;
extern crate color;
extern crate cpu;
extern crate framebuffer;
extern crate framebuffer_drawer;
extern crate getopts;
extern crate hpet;
extern crate rendezvous;
extern crate runqueue;
extern crate scheduler;
extern crate shapes;
extern crate spawn;
extern crate spin;
extern crate task;
extern crate unified_channel;
extern crate window;
use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use color::Color;
use getopts::Options;
use hpet::get_hpet;
use shapes::Coord;
use spawn::new_task_builder;
use spin::Mutex;
use unified_channel::{StringReceiver, StringSender};
use window::Window;
pub fn main(_args: Vec<String>) -> isize {
    {
        let _task_ref = match spawn::new_task_builder(foo_loop, ())
            .name("foo_loop".to_string())
            .spawn()
        {
            Ok(task_ref) => task_ref,
            Err(err) => {
                error!("{}", err);
                error!("failed to spawn shell");
                return -1;
            }
        };
    }

    task::with_current_task(|t| t.block())
        .expect("shell::main(): failed to get current task")
        .expect("shell:main(): failed to block the main shell task");
    scheduler::schedule();

    1
}
pub fn hey() -> Result<(), &'static str> {
    // debug!("Starting the fault task");
    let window_wrap = Window::new(Coord::new(0, 0), 500, 500, color::WHITE);
    let mut window = window_wrap.expect("Window creation failed");

    loop {
        let color = Color::new(0x239B56);
        framebuffer_drawer::draw_rectangle(
            &mut window.framebuffer_mut(),
            Coord::new(0, 0),
            50,
            50,
            framebuffer::Pixel::weight_blend(color::RED.into(), color.into(), 10.0),
        );

        window.render(None)?;
        scheduler::schedule();
    }
}
pub fn foo_loop(mut _dummy: ()) -> Result<(), &'static str> {
    hey()?;
    Ok(())
}

#![no_std]

#[macro_use]
extern crate alloc;
extern crate hpet;
extern crate mouse;
extern crate mouse_data;
extern crate multicore_bringup;
extern crate scheduler;
extern crate spin;
extern crate task;
use alloc::sync::{Arc, Weak};
use core::mem;
use log::info;
use spin::{Mutex, MutexGuard, Once};
use stdio::{
    KeyEventQueue, KeyEventQueueReader, KeyEventQueueWriter, Stdio, StdioReader, StdioWriter,
};

use event_types::{Event, MousePositionEvent};
use keycodes_ascii::{KeyAction, KeyEvent, Keycode};
use mpmc::Queue;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;
use font::{CHARACTER_HEIGHT, CHARACTER_WIDTH};
use hpet::get_hpet;
use memory::{BorrowedSliceMappedPages, EntryFlags, Frame, Mutable, PhysicalAddress};
use mouse_data::MouseEvent;
use task::{ExitValue, JoinableTaskRef, KillReason};
pub static WINDOW_MANAGER: Once<Mutex<WindowManager>> = Once::new();


static MOUSE_POINTER_IMAGE: [[u32; 18]; 11] = {
    const T: u32 = 0xFF0000;
    const C: u32 = 0x000000; // Cursor
    const B: u32 = 0xFFFFFF; // Border
    [
        [B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, T, T],
        [T, B, C, C, C, C, C, C, C, C, C, C, C, C, B, T, T, T],
        [T, T, B, C, C, C, C, C, C, C, C, C, C, B, T, T, T, T],
        [T, T, T, B, C, C, C, C, C, C, C, C, B, T, T, T, T, T],
        [T, T, T, T, B, C, C, C, C, C, C, C, C, B, B, T, T, T],
        [T, T, T, T, T, B, C, C, C, C, C, C, C, C, C, B, B, T],
        [T, T, T, T, T, T, B, C, C, C, C, B, B, C, C, C, C, B],
        [T, T, T, T, T, T, T, B, C, C, B, T, T, B, B, C, B, T],
        [T, T, T, T, T, T, T, T, B, C, B, T, T, T, T, B, B, T],
        [T, T, T, T, T, T, T, T, T, B, B, T, T, T, T, T, T, T],
        [T, T, T, T, T, T, T, T, T, T, B, T, T, T, T, T, T, T],
    ]
};
pub struct TextDisplay {
    width: usize,
    height: usize,
    next_col: usize,
    next_line: usize,
    text: String,
    fg_color: u32,
    bg_color: u32,
    cache: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub width: usize,
    pub height: usize,
    pub x: isize,
    pub y: isize,
}

impl Rect {
    fn new(width: usize, height: usize, x: isize, y: isize) -> Rect {
        Rect {
            width,
            height,
            x,
            y,
        }
    }

    fn start_x(&self) -> isize {
        self.x
    }

    fn end_x(&self) -> isize {
        self.x + self.width as isize
    }

    fn start_y(&self) -> isize {
        self.y
    }

    fn end_y(&self) -> isize {
        self.y + self.height as isize
    }

    fn detect_collision(&self, other: &Rect) -> bool {
        if self.x < other.end_x()
            && self.end_x() > other.x
            && self.y < other.end_y()
            && self.end_y() > other.y
        {
            true
        } else {
            false
        }
    }
}

pub struct FrameBuffer {
    width: usize,
    height: usize,
    buffer: BorrowedSliceMappedPages<u32, Mutable>,
}
impl FrameBuffer {
    fn init_front_buffer() -> Result<FrameBuffer, &'static str> {
        let graphic_info = multicore_bringup::GRAPHIC_INFO.lock();
        if graphic_info.physical_address == 0 {
            return Err("wrong physical address for porthole");
        }
        let vesa_display_phys_start =
            PhysicalAddress::new(graphic_info.physical_address as usize).ok_or("Invalid address");
        let buffer_width = graphic_info.width as usize;
        let buffer_height = graphic_info.height as usize;

        let framebuffer = FrameBuffer::new(
            buffer_width,
            buffer_height,
            Some(vesa_display_phys_start.unwrap()),
        )?;
        Ok(framebuffer)
    }

    fn new(
        width: usize,
        height: usize,
        physical_address: Option<PhysicalAddress>,
    ) -> Result<FrameBuffer, &'static str> {
        let kernel_mmi_ref =
            memory::get_kernel_mmi_ref().ok_or("KERNEL_MMI was not yet initialized!")?;

        let mut vesa_display_flags: EntryFlags =
            EntryFlags::PRESENT | EntryFlags::WRITABLE | EntryFlags::GLOBAL;

        if physical_address.is_some() {
            vesa_display_flags |= EntryFlags::NO_CACHE;
        }
        let size = width * height * core::mem::size_of::<u32>();
        let pages = memory::allocate_pages_by_bytes(size)
            .ok_or("could not allocate pages for a new framebuffer")?;

        let mapped_framebuffer = if let Some(address) = physical_address {
            let frames = memory::allocate_frames_by_bytes_at(address, size)
                .map_err(|_e| "Couldn't allocate frames for the final framebuffer")?;
            kernel_mmi_ref.lock().page_table.map_allocated_pages_to(
                pages,
                frames,
                vesa_display_flags,
            )?
        } else {
            kernel_mmi_ref
                .lock()
                .page_table
                .map_allocated_pages(pages, vesa_display_flags)?
        };

        // obtain a slice reference to the framebuffer's memory
        let buffer = mapped_framebuffer
            .into_borrowed_slice_mut(0, width * height)
            .map_err(|(_mp, s)| s)?;

        Ok(FrameBuffer {
            width,
            height,
            buffer,
        })
    }

    pub fn draw_something(&mut self, x: isize, y: isize, col: u32) {
        if x > 0 && x < self.width as isize && y >0 && y < self.height as isize {
            self.buffer[(self.width * y as usize) + x as usize] = col;
        }
    }

    pub fn get_pixel(&self, x: isize, y: isize) -> u32 {
        self.buffer[(self.width * y as usize) + x as usize]
    }

    pub fn draw_rectangle(&mut self, rect: &Rect) {
        for y in rect.start_y()..rect.end_y() {
            for x in rect.start_x()..rect.end_x() {
                if x > 0 && x < self.width as isize && y > 0 && y < self.height as isize {
                    self.draw_something(x, y, 0xF123999);
                }
            }
        }
    }

    pub fn blank(&mut self) {
        for pixel in self.buffer.iter_mut() {
            *pixel = 0x000000;
        }
    }

    pub fn blank_rect(&mut self, rect: &Rect) {
        for y in rect.y..rect.end_y() {
            for x in rect.x..rect.end_x() {
                self.draw_something(x, y, 0x000000);
            }
        }
    }

    fn copy_window_only(&mut self, window: &MutexGuard<Window>) {
        for y in 0..window.rect.height {
            for x in 0..window.rect.width {
                let pixel = window.frame_buffer.get_pixel(x as isize, y as isize);
                let x = x as isize;
                let y = y as isize;
                if (x + window.rect.x) > 0
                    && (window.rect.x + x) < self.width as isize
                    && (y + window.rect.y) > 0
                    && (y + window.rect.y) < self.height as isize
                {
                    self.draw_something(
                        x as isize + window.rect.x,
                        y as isize + window.rect.y,
                        pixel,
                    );
                }
            }
        }
    }
}

pub fn main(_args: Vec<String>) -> isize {
    let mouse_consumer = Queue::with_capacity(100);
    let mouse_producer = mouse_consumer.clone();
    WindowManager::init();
    mouse::init(mouse_producer).unwrap();

    let _task_ref = match spawn::new_task_builder(port_loop, mouse_consumer)
        .name("port_loop".to_string())
        .spawn()
    {
        Ok(task_ref) => task_ref,
        Err(err) => {
            log::error!("{}", err);
            log::error!("failed to spawn shell");
            return -1;
        }
    };
    // block this task, because it never needs to actually run again
    task::get_my_current_task().unwrap().block().unwrap();
    scheduler::schedule();

    loop {
        log::warn!("BUG: blocked shell task was scheduled in unexpectedly");
    }
}

pub struct WindowManager {
    windows: Vec<Weak<Mutex<Window>>>,
    v_framebuffer: FrameBuffer,
    p_framebuffer: FrameBuffer,
    pub mouse: Rect,
}

impl WindowManager {
    fn init() {
        let p_framebuffer = FrameBuffer::init_front_buffer().unwrap();
        let v_framebuffer =
            FrameBuffer::new(p_framebuffer.width, p_framebuffer.height, None).unwrap();
        let mouse = Rect::new(11, 18, 200, 200);

        let window_manager = WindowManager {
            windows: Vec::new(),
            v_framebuffer,
            p_framebuffer,
            mouse,
        };
        WINDOW_MANAGER.call_once(|| Mutex::new(window_manager));
    }

    fn new_window(dimensions: &Rect) -> Arc<Mutex<Window>> {
        let mut manager = WINDOW_MANAGER.get().unwrap().lock();

        let buffer_width = manager.p_framebuffer.width as usize;
        let buffer_height = manager.p_framebuffer.height as usize;

        let window = Window::new(
            *dimensions,
            FrameBuffer::new(dimensions.width, dimensions.height, None).unwrap(),
        );
        let arc_window = Arc::new(Mutex::new(window));
        manager.windows.push(Arc::downgrade(&arc_window.clone()));
        arc_window
    }

    fn draw_windows(&mut self) {
        for window in self.windows.iter() {
            self.v_framebuffer
                .copy_window_only(&window.upgrade().unwrap().lock());
        }
        for window in self.windows.iter() {
            window.upgrade().unwrap().lock().blank();
        }
    }

    fn draw_mouse(&mut self) {
        let mouse = self.mouse;
        for y in mouse.y..mouse.y + mouse.height as isize{
            for x in mouse.x..mouse.x + mouse.width as isize{
                let color = MOUSE_POINTER_IMAGE[(x - mouse.x) as usize][(y - mouse.y) as usize];
                if color != 0xFF0000{
                    self.v_framebuffer.draw_something(x, y, color);
                }
            }
        }
    }

    fn update(&mut self) {
        self.v_framebuffer.blank();
        self.draw_windows();
        self.draw_mouse();
    }

    fn update_mouse_position(&mut self, x: isize, y: isize) {
        let mut new_pos_x = self.mouse.x + x;
        let mut new_pos_y = self.mouse.y - y;

        // handle left
        if (new_pos_x + (self.mouse.width as isize / 2)) < 0 {
            new_pos_x = self.mouse.x;
        }
        // handle right
        if new_pos_x + (self.mouse.width as isize / 2) > self.v_framebuffer.width as isize {
            new_pos_x = self.mouse.x;
        }

        // handle top
        if new_pos_y < 0 {
            new_pos_y = self.mouse.y;
        }

        // handle bottom
        if new_pos_y + (self.mouse.height as isize / 2) > self.v_framebuffer.height as isize {
            new_pos_y = self.mouse.y;
        }

        self.mouse.x = new_pos_x;
        self.mouse.y = new_pos_y;
    }

    fn drag_windows(&mut self, x: isize, y: isize, mouse_event: &MouseEvent) {
        if mouse_event.buttonact.left_button_hold {
            for window in self.windows.iter_mut() {
                if window
                    .upgrade()
                    .unwrap()
                    .lock()
                    .rect
                    .detect_collision(&Rect::new(
                        self.mouse.width,
                        self.mouse.height,
                        self.mouse.x,
                        self.mouse.y,
                    ))
                {
                    let window_rect = window.upgrade().unwrap().lock().rect;
                    let mut new_pos_x = window_rect.x + x;
                    let mut new_pos_y = window_rect.y - y;

                    //handle left
                    if (new_pos_x + (window_rect.width as isize - 20)) < 0 {
                        new_pos_x = window_rect.x;
                    }

                    //handle right
                    if (new_pos_x + 20) > self.v_framebuffer.width as isize {
                        new_pos_x = window_rect.x;
                    }

                    //handle top
                    if new_pos_y <= 0 {
                        new_pos_y = window_rect.y;
                    }

                    if new_pos_y + 20 > self.v_framebuffer.height as isize {
                        new_pos_y = window_rect.y;
                    }

                    window.upgrade().unwrap().lock().rect.x = new_pos_x;
                    window.upgrade().unwrap().lock().rect.y = new_pos_y;
                }
            }
        } else if mouse_event.buttonact.right_button_hold {
            let pos_x = self.mouse.x;
            let pos_y = self.mouse.y;

            for window in self.windows.iter_mut() {
                if window
                    .upgrade()
                    .unwrap()
                    .lock()
                    .rect
                    .detect_collision(&Rect::new(
                        self.mouse.width,
                        self.mouse.height,
                        self.mouse.x,
                        self.mouse.y,
                    ))
                {
                    window.upgrade().unwrap().lock().rect.width += x as usize;
                    window.upgrade().unwrap().lock().rect.height -= y as usize;
                    window.upgrade().unwrap().lock().resized = true;
                }
            }
        }
    }

    #[inline]
    fn render(&mut self) {
        self.p_framebuffer
            .buffer
            .copy_from_slice(&self.v_framebuffer.buffer);
    }
}

pub struct Window {
    rect: Rect,
    pub frame_buffer: FrameBuffer,
    resized: bool,
}

impl Window {
    fn new(rect: Rect, frame_buffer: FrameBuffer) -> Window {
        Window {
            rect,
            frame_buffer,
            resized: false,
        }
    }

    pub fn blank(&mut self) {
        for pixel in self.frame_buffer.buffer.iter_mut() {
            *pixel = 0x000000;
        }
    }

    pub fn blank_with_color(&mut self, rect: &Rect, col: u32) {
        let start_x = rect.x;
        let end_x = start_x + rect.width as isize;

        let start_y = rect.y;
        let end_y = start_y + rect.height as isize;

        for y in rect.x..rect.height as isize {
            for x in rect.y..rect.width as isize {
                self.draw_something(x as isize, y as isize, col);
            }
        }
    }

    pub fn draw_absolute(&mut self, x: isize, y: isize, col: u32) {
        if x <= self.rect.width as isize && y <= self.rect.height as isize {
            self.draw_something(x, y, col);
        }
    }

    pub fn draw_relative(&mut self, x: isize, y: isize, col: u32) {
        let x = x - self.rect.x;
        let y = y - self.rect.y;

        self.draw_something(x, y, col);
    }

    // TODO: Change the name
    fn draw_something(&mut self, x: isize, y: isize, col: u32) {
        if x >= 0 && y >= 0 {
            self.frame_buffer.buffer[(self.frame_buffer.width * y as usize) + x as usize] = col;
        }
    }

    pub fn draw_rectangle(&mut self, col: u32) {
        // TODO: This should be somewhere else and it should be a function
        if self.resized {
            self.resize_framebuffer();
            self.resized = false;
        }
        for y in 0..self.rect.height {
            for x in 0..self.rect.width {
                self.draw_something(x as isize, y as isize, col);
            }
        }
    }
    pub fn set_position(&mut self, x: isize, y: isize) {
        self.rect.x = x;
        self.rect.y = y;
    }

    fn resize_framebuffer(&mut self) {
        self.frame_buffer = FrameBuffer::new(self.rect.width, self.rect.height, None).unwrap();
    }

    pub fn print_string(
        &mut self,
        _rect: &Rect,
        slice: &str,
        fg_color: u32,
        bg_color: u32,
        column: usize,
        line: usize,
    ) {
        let rect = self.rect;
        let buffer_width = rect.width / CHARACTER_WIDTH;
        let buffer_height = rect.height / CHARACTER_HEIGHT;
        let (x, y) = (rect.x, rect.y);

        let mut curr_line = line;
        let mut curr_column = column;

        let top_left_x = 0;
        let top_left_y = 0;

        let some_slice = slice.as_bytes();

        self.print_ascii_character(96, fg_color, bg_color, &rect, column, line, some_slice);
        /* 
        for byte in slice.bytes() {
            if byte == b'\n' {
                curr_column = column;
                curr_line += 1;

                if curr_line == buffer_height {
                    break;
                }
            } else {
                if curr_column == buffer_width {
                    curr_column = 0;
                    curr_line += 1;

                    if curr_line == buffer_height {
                        break;
                    }
                }
                self.print_ascii_character(byte, fg_color, bg_color, &rect, curr_column, curr_line,some_slice);
                curr_column += 1;
            }
        }
        */
    }

    pub fn print_ascii_character(
        &mut self,
        character: u8,
        fg_color: u32,
        bg_color: u32,
        rect: &Rect,
        column: usize,
        line: usize,
        slice:&[u8],
    ) {
        let start_x = rect.x + (column as isize * CHARACTER_WIDTH as isize);
        let start_y = rect.y + (line as isize * CHARACTER_HEIGHT as isize);

        let buffer_width = self.frame_buffer.width;
        let buffer_height = self.frame_buffer.height;

        let off_set_x: usize = 0;
        let off_set_y: usize = 0;

        let mut j = off_set_x;
        let mut i = off_set_y;
        let mut z = 0;
        let mut index_j = j;
        loop {
            let x = start_x + j as isize;
            let y = start_y + i as isize;
            if j % CHARACTER_WIDTH == 0 {
                    index_j = 0;
            }
            let color = if index_j >= 1 {
                let index = index_j - 1;
                let char_font = font::FONT_BASIC[slice[z] as usize][i];
                index_j +=1;
                if self.get_bit(char_font, index) != 0 {
                    fg_color
                } else {
                    bg_color
                }
            } else {
                index_j +=1;
                bg_color
            };
            self.draw_relative(x, y, color);

            j += 1;
            if j == CHARACTER_WIDTH || j % CHARACTER_WIDTH == 0 ||start_x + j as isize == buffer_width as isize {
                //i += 1;
                if slice.len() >= 1 && z < slice.len() - 1{
                    z +=1;
                }

                if j >= CHARACTER_WIDTH * slice.len() && j % (CHARACTER_WIDTH * slice.len()) == 0 {
                    i+=1;
                    z=0;
                    j = off_set_x;
                }

                if i == CHARACTER_HEIGHT || start_y + i as isize == buffer_height as isize {
                    break;
                }
            }
        }
    }
    fn get_bit(&self, char_font: u8, i: usize) -> u8 {
        char_font & (0x80 >> i)
    }
}

fn port_loop(mouse_consumer: Queue<Event>) -> Result<(), &'static str> {
    let window_manager = WINDOW_MANAGER.get().unwrap();
    //let window = WindowManager::new_window(&Rect::new(100, 100, 100, 100));
    // TODO: There is a bug which causes things to render badly it's probably caused by relative rendering investigate it
    let window_2 = WindowManager::new_window(&Rect::new(400, 400, 0, 0));
    let hpet = get_hpet();
    let mut start = hpet
        .as_ref()
        .ok_or("couldn't get HPET timer")?
        .get_counter();
    let hpet_freq = hpet.as_ref().ok_or("ss")?.counter_period_femtoseconds() as u64;

    let mut x = 0;
    let mut inc = true;
    let mut update = true;
    loop {
        let mut end = hpet
            .as_ref()
            .ok_or("couldn't get HPET timer")?
            .get_counter();
        let mut diff = (end - start) * hpet_freq / 1_000_000_000_000;
        let event_opt = mouse_consumer.pop().or_else(|| {
            scheduler::schedule();
            None
        });

        if let Some(event) = event_opt {
            match event {
                Event::MouseMovementEvent(ref mouse_event) => {
                    let displacement = &mouse_event.displacement;
                    let mut x = (displacement.x as i8) as isize;
                    let mut y = (displacement.y as i8) as isize;
                    while let Some(next_event) = mouse_consumer.pop() {
                        match next_event {
                            Event::MouseMovementEvent(ref next_mouse_event) => {
                                //log::info!("next mouse event is {:?}",next_mouse_event);
                                //log::info!("EE");
                                if next_mouse_event.mousemove.scrolling_up
                                    == mouse_event.mousemove.scrolling_up
                                    && next_mouse_event.mousemove.scrolling_down
                                        == mouse_event.mousemove.scrolling_down
                                    && next_mouse_event.buttonact.left_button_hold
                                        == mouse_event.buttonact.left_button_hold
                                    && next_mouse_event.buttonact.right_button_hold
                                        == mouse_event.buttonact.right_button_hold
                                    && next_mouse_event.buttonact.fourth_button_hold
                                        == mouse_event.buttonact.fourth_button_hold
                                    && next_mouse_event.buttonact.fifth_button_hold
                                        == mouse_event.buttonact.fifth_button_hold
                                {
                                    x += (next_mouse_event.displacement.x as i8) as isize;
                                    y += (next_mouse_event.displacement.y as i8) as isize;
                                }
                            }
                            _ => {
                                break;
                            }
                        }
                    }
                    if x != 0 || y != 0 {
                        window_manager.lock().update_mouse_position(x, y);
                        window_manager.lock().drag_windows(x, y, &mouse_event);
                    }
                }
                _ => (),
            }
        }
        //virt_buffer.draw_rectangle(&window.mouse);

        if update {
            //window.lock().set_position(x, 100);
            window_2.lock().draw_rectangle(0x4a4a4a);
            //window.lock().draw_rectangle(0x987454);
        }
        if diff >= 16 {
            update = false;
            window_2.lock().print_string(
                &Rect::new(100, 200, 10, 10),
                "Hello this is theseus who is talking",
                0x123456,
                0xFFF111,
                2,
                2,
            );
            //window_2.lock().draw_absolute(0, 0, 0xFFFFFF);
            window_manager.lock().update();
            window_manager.lock().render();
            start = hpet.as_ref().unwrap().get_counter();
            if x == 500 {
                inc = false;
            }

            if inc {
                x += 1;
            }
            if x == 0 {
                inc = true;
            }
            if !inc {
                x -= 1;
            }
            update = true;
        }
    }
    Ok(())
}

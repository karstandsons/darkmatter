//! vpointer <x> <y> <layout width> <layout height> [move|click|right|scroll <steps>|drag <x2> <y2>]
//!
//! Coordinates are logical pixels in the compositor's layout. Don't call this
//! directly: dev/click.sh checks that Darkmatter is focused and the point is in it.

use std::{thread, time::Duration};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

struct State;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize| args.get(i).and_then(|a| a.parse::<u32>().ok()).unwrap_or_else(|| usage());
    let (x, y, w, h) = (num(0), num(1), num(2), num(3));
    let action = args.get(4).map_or("move", String::as_str);

    let conn = Connection::connect_to_env().expect("no Wayland session");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("no seat");
    let manager: ZwlrVirtualPointerManagerV1 =
        globals.bind(&qh, 1..=2, ()).expect("compositor lacks zwlr_virtual_pointer_v1");
    let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());

    let mut t = 0u32;
    let mut tick = || {
        t += 10;
        t
    };
    let flush = |queue: &mut wayland_client::EventQueue<State>| {
        queue.roundtrip(&mut State).expect("roundtrip");
        // Give the app a frame to see the cursor (hover) before the next step.
        thread::sleep(Duration::from_millis(60));
    };

    // Nudge first: if the cursor is already at the target there'd be no motion
    // event, and a window opened under it wouldn't know where the cursor is.
    for x in [x.saturating_sub(1), x] {
        pointer.motion_absolute(tick(), x, y, w, h);
        pointer.frame();
        flush(&mut queue);
    }
    match action {
        "move" => {}
        "click" | "right" => {
            let button = if action == "click" { BTN_LEFT } else { BTN_RIGHT };
            pointer.button(tick(), button, ButtonState::Pressed);
            pointer.frame();
            flush(&mut queue);
            pointer.button(tick(), button, ButtonState::Released);
            pointer.frame();
        }
        "drag" => {
            // Press here, glide to (x2, y2) in small steps so the app sees the motion, release.
            let (x2, y2) = (num(5), num(6));
            pointer.button(tick(), BTN_LEFT, ButtonState::Pressed);
            pointer.frame();
            flush(&mut queue);
            for i in 1..=10 {
                let at = |a: u32, b: u32| (a as i64 + (b as i64 - a as i64) * i / 10) as u32;
                pointer.motion_absolute(tick(), at(x, x2), at(y, y2), w, h);
                pointer.frame();
                flush(&mut queue);
            }
            pointer.button(tick(), BTN_LEFT, ButtonState::Released);
            pointer.frame();
        }
        "scroll" => {
            // Positive steps scroll down, like a mouse wheel.
            let steps: i32 = args.get(5).and_then(|a| a.parse().ok()).unwrap_or_else(|| usage());
            for _ in 0..steps.unsigned_abs() {
                let dir = steps.signum();
                let time = tick();
                pointer.axis_source(AxisSource::Wheel);
                pointer.axis_discrete(time, Axis::VerticalScroll, 15.0 * dir as f64, dir);
                pointer.frame();
                flush(&mut queue);
            }
        }
        _ => usage(),
    }
    flush(&mut queue);
    pointer.destroy();
    queue.roundtrip(&mut State).ok();
}

fn usage() -> ! {
    eprintln!("usage: vpointer <x> <y> <layout width> <layout height> [move|click|right|scroll <steps>|drag <x2> <y2>]");
    std::process::exit(2)
}

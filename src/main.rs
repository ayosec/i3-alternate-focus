use std::collections::VecDeque;
use std::env;
use std::error::Error;
use std::io;
use std::io::{BufRead, BufReader, Write};
use std::mem;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use i3ipc::event::inner::WindowChange;
use i3ipc::event::Event;
use i3ipc::{I3Connection, I3EventListener, Subscription};

mod xprop;

/// Min. time with focus required to keep a window in the queue.
const MIN_FOCUS: Duration = Duration::from_secs(2);

static BUFFER_SIZE: usize = 100;

const SOCKET_PATH_PROP: &str = "_I3_ALTERNATE_FOCUS_SOCKET";

const SWITCH_COMMAND: &[u8] = b"switch";
const DEBUG_COMMAND: &[u8] = b"debug";

#[derive(Debug)]
struct Window {
    id: i64,
    just_switched: bool,
    focused: Instant,
}

impl Window {
    fn new(id: i64) -> Window {
        Window {
            id,
            just_switched: false,
            focused: Instant::now(),
        }
    }
}

fn focus_nth(windows: &VecDeque<Window>, n: usize) -> Result<(), Box<dyn Error>> {
    let mut conn = I3Connection::connect().unwrap();
    let mut k = n;

    // Start from the nth window and try to change focus until it succeeds
    // (so that it skips windows which no longer exist)
    while let Some(win) = windows.get(k) {
        let wid = win.id;
        let r = conn.run_command(format!("[con_id={}] focus", wid).as_str())?;

        if let Some(o) = r.outcomes.get(0) {
            if o.success {
                return Ok(());
            }
        }

        k += 1;
    }

    Err(From::from(format!("Last {}nth window unavailable", n)))
}

fn cmd_server(windows: Arc<Mutex<VecDeque<Window>>>) {
    let socket = {
        let mut base = match env::var("XDG_RUNTIME_DIR") {
            Ok(path) => PathBuf::from(path),
            Err(_) => PathBuf::from("/tmp"),
        };

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let name = format!(
            "i3-alternate-focus.{}.{}.sock",
            std::process::id(),
            timestamp
        );

        base.push(&name);
        base
    };

    xprop::set(SOCKET_PATH_PROP, &socket.to_string_lossy()).expect("Set xprop");

    // Listen to client commands
    let listener = UnixListener::bind(socket).unwrap();

    for stream in listener.incoming() {
        if let Ok(mut stream) = stream {
            let windows = windows.clone();

            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap()).lines();
                let line = reader.next();
                match line {
                    Some(Ok(line)) if line.as_bytes() == SWITCH_COMMAND => {
                        let mut winc = windows.lock().unwrap();

                        // Ignore MIN_FOCUS if we alternate focus between two
                        // windows
                        winc.front_mut()
                            .iter_mut()
                            .for_each(|win| win.just_switched = true);

                        let _ = focus_nth(&winc, 1);
                    }

                    Some(Ok(line)) if line.as_bytes() == DEBUG_COMMAND => {
                        let winc = windows.lock().unwrap();
                        let _ = write!(&mut stream, "{:#?}\n", winc);
                    }

                    _ => {
                        let _ = stream.write_all(b"Invalid command\n");
                    }
                }
            });
        }
    }
}

fn get_focused_window() -> Result<i64, ()> {
    let mut conn = I3Connection::connect().unwrap();
    let mut node = conn.get_tree().unwrap();

    while !node.focused {
        let fid = node.focus.into_iter().nth(0).ok_or(())?;
        node = node
            .nodes
            .into_iter()
            .filter(|n| n.id == fid)
            .nth(0)
            .ok_or(())?;
    }

    Ok(node.id)
}

fn focus_server() {
    let mut listener = I3EventListener::connect().unwrap();
    let windows = Arc::new(Mutex::new(VecDeque::new()));
    let windowsc = Arc::clone(&windows);

    // Add the current focused window to bootstrap the list
    get_focused_window()
        .map(|wid| {
            let mut windows = windows.lock().unwrap();
            windows.push_front(Window::new(wid));
        })
        .ok();

    thread::spawn(|| cmd_server(windowsc));

    // Listens to i3 event
    let subs = [Subscription::Window];
    listener.subscribe(&subs).unwrap();

    for event in listener.listen() {
        match event.unwrap() {
            Event::WindowEvent(e) => {
                if let WindowChange::Focus = e.change {
                    let mut windows = windows.lock().unwrap();

                    if let Some(win) = windows.front_mut() {
                        if !mem::replace(&mut win.just_switched, false) {
                            if win.focused.elapsed() < MIN_FOCUS {
                                let _ = windows.pop_front();
                            }
                        }
                    }

                    // dedupe, push front and truncate
                    windows.retain(|v| v.id != e.container.id);
                    windows.push_front(Window::new(e.container.id));
                    windows.truncate(BUFFER_SIZE);
                }
            }
            _ => unreachable!(),
        }
    }
}

fn focus_client(command: &str) {
    let socket_path = xprop::get(SOCKET_PATH_PROP).expect("Get xprop");
    let mut stream = UnixStream::connect(socket_path).unwrap();

    write!(&mut stream, "{}\n", command).expect("Write to socket");
    io::copy(&mut stream, &mut io::stdout().lock()).expect("Copy server output");
}

fn main() {
    match env::args().skip(1).next() {
        Some(arg) if arg == "server" => {
            focus_server();
        }
        Some(arg) => {
            focus_client(&arg);
        }
        _ => {
            eprintln!("Expected argument: server, switch, debug");
        }
    }
}

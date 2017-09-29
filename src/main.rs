#[macro_use]
extern crate serde_derive;

extern crate clap;
extern crate i3ipc;
extern crate rmp_serde;
extern crate serde;

use std::env;
use std::error::Error;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use clap::{App, Arg, SubCommand};
use i3ipc::{I3Connection, I3EventListener, Subscription};
use i3ipc::event::Event;
use i3ipc::event::inner::WindowChange;
use std::os::unix::net::{UnixListener, UnixStream};

static BUFFER_SIZE: usize = 10;

fn socket_filename() -> String {
    env::var("HOME").unwrap() + "/.local/share/i3-focus-last.sock"
}

#[derive(Serialize, Deserialize, Debug)]
enum Cmd {
    SwitchTo(usize),
}

fn cmd_server(windows: Arc<Mutex<VecDeque<i64>>>) {
    let socket = socket_filename();
    let socket = Path::new(&socket);

    if socket.exists() {
        fs::remove_file(&socket).unwrap();
    }

    let listener = UnixListener::bind(socket).unwrap();

    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            let winc = windows.clone();

            thread::spawn(move || {
                let mut conn = I3Connection::connect().unwrap();
                let cmd = rmp_serde::from_read::<_, Cmd>(stream);

                if let Ok(Cmd::SwitchTo(nth)) = cmd {
                    let winc = winc.lock().unwrap();

                    if let Some(wid) = winc.get(nth) {
                        conn.run_command(format!("[con_id={}] focus", wid).as_str());
                    }
                }
            });
        }
    }
}

fn focus_server() {
    let mut listener = I3EventListener::connect().unwrap();
    let windows = Arc::new(Mutex::new(VecDeque::new()));
    let windowsc = Arc::clone(&windows);

    thread::spawn(|| cmd_server(windowsc));

    // Listens to i3 event
    let subs = [Subscription::Window];
    listener.subscribe(&subs).unwrap();

    for event in listener.listen() {
        match event.unwrap() {
            Event::WindowEvent(e) => {
                if let WindowChange::Focus = e.change {
                    //let mut windows = Mutex::make_mut(&mut windows);
                    let mut windows = windows.lock().unwrap();

                    windows.push_front(e.container.id);
                    windows.truncate(BUFFER_SIZE);
                }
            }
            _ => unreachable!()
        }
    }
}

fn focus_client(nth_window: usize) {
    let mut stream = UnixStream::connect(socket_filename()).unwrap();

    rmp_serde::to_vec(&Cmd::SwitchTo(nth_window))
        .map(move |b| stream.write_all(b.as_slice()));
}

fn main() {
    let matches = App::new("i3-focus-last")
                          .subcommand(SubCommand::with_name("server")
                                     .about("Run in server mode"))
                          .arg(Arg::with_name("nth_window")
                              .short("n")
                              .value_name("N")
                              .help("nth widow to focus")
                              .default_value("1")
                              .validator(|v| v.parse::<usize>().map_err(|e| String::from(e.description()))
                                                               .and_then(|v| if v > 0 && v <= BUFFER_SIZE { Ok(v) }
                                                                           else { Err(String::from("invalid n")) }
                                                                        )
                                                               .map(|_| ())
                                                             ))
                          .get_matches();

    if let Some(_) = matches.subcommand_matches("server") {
        focus_server();
    } else {
        focus_client(matches.value_of("nth_window").unwrap().parse().unwrap());
    }
}
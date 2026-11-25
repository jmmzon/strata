// SPDX-License-Identifier: MIT

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn links_a_1_x_engine() {
    let version = engine_version();
    assert!(version.starts_with("1."), "unexpected engine version {version}");
}

#[test]
fn renders_written_text() {
    let mut terminal = Terminal::new(40, 5).expect("terminal");
    terminal.vt_write(b"hello \x1b[1;32mworld\x1b[0m\r\nsecond line");
    let text = plain_text(&terminal).expect("format");
    assert_eq!(text, "hello world\nsecond line");
}

#[test]
fn answers_device_attribute_queries_through_the_pty_callback() {
    let replies = Rc::new(RefCell::new(Vec::<u8>::new()));
    let sink = replies.clone();
    let mut terminal = Terminal::new(40, 5).expect("terminal");
    terminal
        .on_pty_write(move |_, data| sink.borrow_mut().extend_from_slice(data))
        .expect("callback");
    terminal.vt_write(b"\x1b[c");
    let reply = replies.borrow();
    assert!(reply.starts_with(b"\x1b[?"), "no DA1 reply: {reply:?}");
}

#[test]
fn reports_the_osc7_working_directory() {
    let mut terminal = Terminal::new(40, 5).expect("terminal");
    terminal.vt_write(b"\x1b]7;file://myhost.example.com/home/alice/src\x1b\\");
    let pwd = terminal.pwd().expect("pwd");
    assert!(
        String::from_utf8_lossy(pwd).ends_with("/home/alice/src"),
        "pwd {pwd:?}"
    );
}

// SPDX-License-Identifier: MIT

use super::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::frame::FrameBuilder;

const SIZE: Size = Size {
    cols: 40,
    rows: 5,
    cell_width_px: 8,
    cell_height_px: 16,
};

#[test]
fn links_a_1_x_engine() {
    let version = engine_version();
    assert!(
        version.starts_with("1."),
        "unexpected engine version {version}"
    );
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

#[test]
fn parses_osc7_reports() {
    let cases: [(&[u8], Option<&str>); 5] = [
        (
            b"file://myhost.example.com/home/alice/my%20src",
            Some("/home/alice/my src"),
        ),
        (b"file:///tmp", Some("/tmp")),
        (b"kitty-shell-cwd://myhost.example.com/srv", Some("/srv")),
        (b"/var/log", Some("/var/log")),
        (b"file://myhost.example.com/bad%2", None),
    ];
    for (report, expected) in cases {
        assert_eq!(
            osc7::path(report),
            expected.map(PathBuf::from),
            "{report:?}"
        );
    }
}

fn sh(script: &str) -> ShellOptions {
    ShellOptions {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        ..ShellOptions::default()
    }
}

/// Pumps until `done` sees the screen text, the shell hangs up, or 10 s pass.
fn pump_until(session: &mut Session, done: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = plain_text(session.terminal()).expect("format");
        if done(&text) {
            return text;
        }
        assert!(Instant::now() < deadline, "timed out; screen:\n{text}");
        wait_readable(session);
        let pumped = session.pump().expect("pump");
        if pumped.hung_up {
            return plain_text(session.terminal()).expect("format");
        }
    }
}

fn wait_readable(session: &Session) {
    let fd = session.fd();
    let mut fds = [rustix::event::PollFd::new(
        &fd,
        rustix::event::PollFlags::IN,
    )];
    let timeout = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 50_000_000,
    };
    let _ = rustix::event::poll(&mut fds, Some(&timeout));
}

#[test]
fn runs_the_shell_on_a_terminal_of_the_requested_size() {
    let mut session =
        Session::spawn(&sh("stty size; echo TERM=$TERM; read l"), SIZE).expect("spawn");
    let text = pump_until(&mut session, |text| text.contains("TERM="));
    assert!(text.contains("5 40"), "{text}");
    assert!(text.contains(&format!("TERM={TERM_FALLBACK}")), "{text}");
}

#[test]
fn sends_input_and_reports_resizes_to_the_shell() {
    let script = "read l; echo got:$l; stty size; read l";
    let mut session = Session::spawn(&sh(script), SIZE).expect("spawn");
    let bigger = Size {
        cols: 60,
        rows: 8,
        ..SIZE
    };
    session.resize(bigger).expect("resize");
    session.write(b"abc\n").expect("write");
    let text = pump_until(&mut session, |text| text.contains("8 60"));
    assert!(text.contains("got:abc"), "{text}");
    assert_eq!(session.size(), bigger);
}

#[test]
fn reports_the_exit_once_the_shell_hangs_up() {
    let mut session = Session::spawn(&sh("echo bye; exit 3"), SIZE).expect("spawn");
    pump_until(&mut session, |_| false);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = session.exit_status() {
            break status;
        }
        assert!(Instant::now() < deadline, "no exit status");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(3));
}

#[test]
fn answers_the_shell_s_terminal_queries() {
    // The shell asks for the cursor position and prints the raw answer.
    let script =
        "stty raw -echo; printf '\\033[6n'; dd bs=1 count=6 2>/dev/null | od -c | head -1; read l";
    let mut session = Session::spawn(&sh(script), SIZE).expect("spawn");
    let text = pump_until(&mut session, |text| text.contains('R'));
    assert!(text.contains("033   [   1   ;   1   R"), "{text}");
}

#[test]
fn tells_a_foreground_job_from_the_idle_shell() {
    let options = ShellOptions {
        program: Some("/bin/sh".into()),
        args: vec!["-i".into()],
        env: vec![("PS1".into(), "PROMPT> ".into())],
        ..ShellOptions::default()
    };
    let mut session = Session::spawn(&options, SIZE).expect("spawn");
    pump_until(&mut session, |text| text.contains("PROMPT>"));
    assert!(!session.has_foreground_job());
    session.write(b"sleep 30\n").expect("write");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !session.has_foreground_job() {
        assert!(Instant::now() < deadline, "sleep never took the terminal");
        std::thread::sleep(Duration::from_millis(20));
        session.pump().expect("pump");
    }
}

#[test]
fn falls_back_to_the_shell_s_directory_without_osc7() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = ShellOptions {
        cwd: Some(dir.path().to_path_buf()),
        ..sh("echo started; read l")
    };
    let mut session = Session::spawn(&options, SIZE).expect("spawn");
    pump_until(&mut session, |text| text.contains("started"));
    let expected = dir.path().canonicalize().expect("canonical");
    assert_eq!(session.cwd(), Some(expected));
}

/// A `share/ghostty` tree and a terminfo directory, and a `fish` that is
/// really `sh`, to check what a fish session is given.
fn fake_ghostty(root: &Path) -> ShellOptions {
    let resources = root.join("ghostty");
    std::fs::create_dir_all(resources.join("shell-integration/fish/vendor_conf.d")).expect("dirs");
    let terminfo = root.join("terminfo");
    std::fs::create_dir_all(terminfo.join("x")).expect("dirs");
    std::fs::write(terminfo.join("x").join(TERM_GHOSTTY), b"").expect("entry");
    let fish = root.join("fish");
    std::os::unix::fs::symlink("/bin/sh", &fish).expect("symlink");
    ShellOptions {
        program: Some(fish.into()),
        args: vec![
            "-c".into(),
            "echo T=$TERM; echo F=$GHOSTTY_SHELL_FEATURES; echo X=${XDG_DATA_DIRS%%:*}; read l"
                .into(),
        ],
        ghostty_resources: Some(resources),
        terminfo: Some(terminfo),
        ..ShellOptions::default()
    }
}

#[test]
fn starts_fish_with_ghostty_integration_and_terminfo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = fake_ghostty(dir.path());
    let mut session = Session::spawn(&options, Size { cols: 120, ..SIZE }).expect("spawn");
    let text = pump_until(&mut session, |text| text.contains("X="));
    assert!(text.contains(&format!("T={TERM_GHOSTTY}")), "{text}");
    assert!(text.contains("F=cursor,sudo,title,ssh-env"), "{text}");
    let integration = dir.path().join("ghostty/shell-integration");
    assert!(
        text.contains(&format!("X={}", integration.display())),
        "{text}"
    );
}

fn frame_of(
    terminal: &mut Terminal<'static, 'static>,
    builder: &mut FrameBuilder,
    frame: &mut Frame,
    bytes: &[u8],
) -> bool {
    terminal.vt_write(bytes);
    builder.update(terminal, frame).expect("frame")
}

#[test]
fn splits_rows_into_styled_runs() {
    let mut terminal = Terminal::new(20, 3).expect("terminal");
    let mut builder = FrameBuilder::new().expect("builder");
    let mut frame = Frame::default();
    frame_of(
        &mut terminal,
        &mut builder,
        &mut frame,
        "\x1b[1;31mred\x1b[0m 漢x   ".as_bytes(),
    );
    let runs = &frame.rows[0].runs;
    let texts: Vec<_> = runs
        .iter()
        .map(|run| (run.col, run.cells, run.text.as_str()))
        .collect();
    assert_eq!(
        texts,
        [(0, 3, "red"), (3, 1, " "), (4, 2, "漢"), (6, 1, "x")]
    );
    assert!(runs[0].style.bold && !runs[1].style.bold);
    assert_ne!(runs[0].style.fg, frame.foreground);
    assert_eq!(runs[1].style.fg, frame.foreground);
    assert!(frame.rows[1].runs.is_empty());
    assert_eq!(
        frame.cursor.map(|cursor| (cursor.col, cursor.row)),
        Some((10, 0))
    );
}

#[test]
fn applies_inverse_video() {
    let mut terminal = Terminal::new(20, 3).expect("terminal");
    let mut builder = FrameBuilder::new().expect("builder");
    let mut frame = Frame::default();
    frame_of(&mut terminal, &mut builder, &mut frame, b"\x1b[7mrev");
    let style = frame.rows[0].runs[0].style;
    assert_eq!(
        (style.fg, style.bg),
        (frame.background, Some(frame.foreground))
    );
}

#[test]
fn keeps_row_versions_until_their_content_changes() {
    let mut terminal = Terminal::new(20, 3).expect("terminal");
    let mut builder = FrameBuilder::new().expect("builder");
    let mut frame = Frame::default();
    frame_of(
        &mut terminal,
        &mut builder,
        &mut frame,
        b"one\r\ntwo\r\nthree",
    );
    let before: Vec<_> = frame.rows.iter().map(|row| (row.id, row.version)).collect();

    assert!(
        !frame_of(&mut terminal, &mut builder, &mut frame, b""),
        "nothing changed"
    );
    frame_of(&mut terminal, &mut builder, &mut frame, b"!");
    assert_eq!(frame.rows[0].version, before[0].1);
    assert_eq!(frame.rows[1].version, before[1].1);
    assert_ne!(frame.rows[2].version, before[2].1);

    // Scrolling moves "two" to the top as the same row, unchanged.
    let edited = frame.rows[1].version;
    frame_of(&mut terminal, &mut builder, &mut frame, b"\r\nfour");
    assert_eq!(frame.rows[0].id, before[1].0);
    assert_eq!(frame.rows[0].version, edited);
    assert_eq!(frame.rows[0].runs[0].text, "two");
}

#[test]
fn draws_default_text_in_the_chosen_colors() {
    let mut session = Session::spawn(&sh("printf plain; read l"), SIZE).expect("spawn");
    let colors = DefaultColors {
        foreground: Some(Rgb { r: 1, g: 2, b: 3 }),
        background: Some(Rgb {
            r: 250,
            g: 250,
            b: 240,
        }),
        ..DefaultColors::default()
    };
    session.set_default_colors(&colors).expect("colors");
    pump_until(&mut session, |text| text.contains("plain"));
    let (frame, _) = session.frame().expect("frame");
    assert_eq!(frame.rows[0].runs[0].style.fg, Rgb { r: 1, g: 2, b: 3 });
    assert_eq!(
        frame.background,
        Rgb {
            r: 250,
            g: 250,
            b: 240
        }
    );
}

#[test]
fn scrolls_the_viewport_through_the_scrollback() {
    let mut terminal = Terminal::new(20, 3).expect("terminal");
    let mut builder = FrameBuilder::new().expect("builder");
    let mut frame = Frame::default();
    frame_of(
        &mut terminal,
        &mut builder,
        &mut frame,
        b"a\r\nb\r\nc\r\nd\r\ne",
    );
    terminal
        .scroll_viewport(ScrollViewport::Top)
        .expect("scroll");
    builder.update(&terminal, &mut frame).expect("frame");
    let top: Vec<_> = frame
        .rows
        .iter()
        .map(|row| row.runs[0].text.as_str())
        .collect();
    assert_eq!(top, ["a", "b", "c"]);
    assert!(frame.cursor.is_none(), "the cursor is below the viewport");
}

const THEME: &str = "palette = 1=#ba1a1a\npalette = 15 = #1e1c13\nbackground = #fff9ed\n";

#[test]
fn reads_the_palette_and_font_from_the_ghostty_config() {
    let cases: [(&str, bool, Option<Rgb>); 4] = [
        (
            "theme = noctalia\n",
            true,
            Some(Rgb {
                r: 0xba,
                g: 0x1a,
                b: 0x1a,
            }),
        ),
        (
            "theme = light:noctalia,dark:other\n",
            false,
            Some(Rgb {
                r: 0xba,
                g: 0x1a,
                b: 0x1a,
            }),
        ),
        ("theme = light:noctalia,dark:other\n", true, None),
        (
            "theme = noctalia\npalette = 1=#00ff00\n",
            true,
            Some(Rgb {
                r: 0,
                g: 0xff,
                b: 0,
            }),
        ),
    ];
    for (config, dark, red) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("themes")).expect("themes");
        std::fs::write(dir.path().join("themes/noctalia"), THEME).expect("theme");
        let config = format!(
            "# comment\nfont-family = \"Xenon Code\"\nfont-family = Noto Sans Mono\nfont-size = 13.5\n{config}"
        );
        std::fs::write(dir.path().join("config.ghostty"), config).expect("config");
        let look = ghostty::read_look(dir.path(), None, dark);
        assert_eq!(look.palette.map(|palette| palette.0[1]), red, "{dark}");
        assert_eq!(look.font_family.as_deref(), Some("Xenon Code"));
        assert_eq!(look.font_size, Some(13.5));
    }
}

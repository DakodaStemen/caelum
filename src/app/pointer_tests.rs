//! Mouse behaviour of terminal panes, driven through the real app UI with
//! synthetic pointer events (no display server or GPU needed).
use super::*;
use alacritty_terminal::term::TermMode;
use egui_kittest::Harness;

type H<'a> = Harness<'a, Option<App>>;

fn harness<'a>(profile: &Path) -> H<'a> {
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000., 640.))
        .with_step_dt(1. / 60.)
        .with_max_steps(8)
        .build_state(
            |ctx, app: &mut Option<App>| {
                if let Some(app) = app {
                    app.frame(ctx);
                }
            },
            None,
        );
    let ctx = h.ctx.clone();
    let mut app = App::new(
        &ctx,
        profile.to_path_buf(),
        State::default(),
        String::new(),
        None,
        profile.join("config.toml"),
        SystemLook::default(),
        false,
    );
    app.test_shell = true;
    *h.state_mut() = Some(app);
    h
}

fn session<'s>(h: &'s mut H<'_>) -> &'s mut Session {
    let app = h.state_mut().as_mut().unwrap();
    app.terminals.values_mut().next().expect("a terminal")
}

/// Steps the UI until `cond` holds for the terminal, or panics after 15 s.
fn until(h: &mut H<'_>, what: &str, cond: impl Fn(&Session) -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        h.step();
        if h.state_mut()
            .as_mut()
            .unwrap()
            .terminals
            .values()
            .next()
            .is_some_and(&cond)
        {
            return;
        }
        let screen = h
            .state_mut()
            .as_mut()
            .unwrap()
            .terminals
            .values()
            .next()
            .map(Session::text)
            .unwrap_or_default();
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}:\n{screen}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts the app, waits for the prompt and runs `command` in the shell.
fn run<'a>(dir: &Path, command: &str) -> H<'a> {
    let mut h = harness(dir);
    until(&mut h, "prompt", |s| s.text().contains('$'));
    session(&mut h).write(format!("{command}\r").into_bytes());
    h
}

/// Screen row whose text starts with `prefix`, after it appears.
fn row_of(h: &mut H<'_>, prefix: &str) -> usize {
    until(h, prefix, |s| {
        s.text().lines().any(|l| l.starts_with(prefix))
    });
    session(h)
        .text()
        .lines()
        .position(|l| l.starts_with(prefix))
        .unwrap()
}

/// A point inside the cell, `fx` of the way across it.
fn at(h: &mut H<'_>, col: usize, row: usize, fx: f32) -> Pos2 {
    let r = session(h).cell_rect(col, row).expect("drawn");
    Pos2::new(r.left() + r.width() * fx, r.center().y)
}

fn modifiers(h: &mut H<'_>, m: Modifiers) {
    h.input_mut().modifiers = m;
}

fn move_to(h: &mut H<'_>, pos: Pos2) {
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    h.step();
}

fn button(h: &mut H<'_>, pos: Pos2, button: egui::PointerButton, pressed: bool) {
    let modifiers = h.input_mut().modifiers;
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    h.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers,
    });
    h.step();
}

fn click(h: &mut H<'_>, pos: Pos2) {
    button(h, pos, egui::PointerButton::Primary, true);
    button(h, pos, egui::PointerButton::Primary, false);
}

fn drag(h: &mut H<'_>, from: Pos2, to: Pos2) {
    move_to(h, from);
    button(h, from, egui::PointerButton::Primary, true);
    for i in 1..=8 {
        move_to(h, from + (to - from) * (i as f32 / 8.));
    }
    button(h, to, egui::PointerButton::Primary, false);
}

fn selection(h: &mut H<'_>) -> Option<String> {
    session(h).selection()
}

/// Waits a little past the double-click window so the next press is a new click.
fn pause(h: &mut H<'_>) {
    for _ in 0..40 {
        h.step();
    }
}

#[test]
fn drag_selects_exactly_the_dragged_text() {
    let d = tempfile::tempdir().unwrap();
    let mut h = run(d.path(), "echo HELLO_SELECTION_WORLD tail");
    let row = row_of(&mut h, "HELLO_SELECTION_WORLD");
    let from = at(&mut h, 0, row, 0.2);
    let to = at(&mut h, 20, row, 0.8);
    drag(&mut h, from, to);
    assert_eq!(selection(&mut h).as_deref(), Some("HELLO_SELECTION_WORLD"));
    // A plain click clears it.
    pause(&mut h);
    let elsewhere = at(&mut h, 3, row, 0.5);
    click(&mut h, elsewhere);
    assert_eq!(selection(&mut h), None);
    // Dragging right to left selects the same text.
    pause(&mut h);
    drag(&mut h, to, from);
    assert_eq!(selection(&mut h).as_deref(), Some("HELLO_SELECTION_WORLD"));
}

#[test]
fn double_click_selects_a_word_and_triple_click_the_line() {
    let d = tempfile::tempdir().unwrap();
    let mut h = run(d.path(), "echo 'alpha https://x.example/a:42 gamma'");
    let row = row_of(&mut h, "alpha https");
    let word = at(&mut h, 12, row, 0.5);
    click(&mut h, word);
    click(&mut h, word);
    assert_eq!(
        selection(&mut h).as_deref(),
        Some("https://x.example/a:42"),
        "a URL with a port-like suffix is one word"
    );
    click(&mut h, word);
    assert_eq!(
        selection(&mut h).as_deref().map(str::trim_end),
        Some("alpha https://x.example/a:42 gamma")
    );
    // Shift+click extends the selection to a point on the next line.
    pause(&mut h);
    let start = at(&mut h, 0, row, 0.2);
    let end = at(&mut h, 4, row, 0.8);
    drag(&mut h, start, end);
    assert_eq!(selection(&mut h).as_deref(), Some("alpha"));
    pause(&mut h);
    modifiers(&mut h, Modifiers::SHIFT);
    let further = at(&mut h, 9, row, 0.8);
    click(&mut h, further);
    modifiers(&mut h, Modifiers::NONE);
    assert_eq!(selection(&mut h).as_deref(), Some("alpha http"));
}

#[test]
fn dragging_past_the_top_scrolls_back_and_keeps_selecting() {
    let d = tempfile::tempdir().unwrap();
    let mut h = run(d.path(), "seq 1 300; echo END_OF_SEQ");
    let row = row_of(&mut h, "END_OF_SEQ");
    let from = at(&mut h, 3, row, 0.8);
    move_to(&mut h, from);
    button(&mut h, from, egui::PointerButton::Primary, true);
    let above = Pos2::new(from.x, 2.);
    for _ in 0..30 {
        move_to(&mut h, above);
    }
    let offset = lock_offset(&mut h);
    button(&mut h, above, egui::PointerButton::Primary, false);
    assert!(offset > 0, "the view scrolled back");
    let text = selection(&mut h).unwrap();
    assert!(
        text.ends_with("END_OF_SEQ") || text.ends_with("END_"),
        "{text:?}"
    );
    assert!(text.starts_with(char::is_numeric), "{text:?}");
    assert!(text.lines().count() > session(&mut h).text().lines().count());
}

fn lock_offset(h: &mut H<'_>) -> usize {
    use alacritty_terminal::grid::Dimensions as _;
    let s = session(h);
    let term = s.term.lock().unwrap();
    let _ = term.screen_lines();
    term.grid().display_offset()
}

fn mode(h: &mut H<'_>) -> TermMode {
    *session(h).term.lock().unwrap().mode()
}

#[test]
fn programs_with_mouse_reporting_get_presses_drags_and_releases() {
    let d = tempfile::tempdir().unwrap();
    let mut h = run(d.path(), "printf '\\033[?1002h\\033[?1006h'; cat");
    until(&mut h, "mouse mode", |s| {
        s.term.lock().unwrap().mode().contains(TermMode::MOUSE_DRAG)
    });
    let row = 5;
    let from = at(&mut h, 2, row, 0.5);
    let to = at(&mut h, 3, row, 0.5);
    drag(&mut h, from, to);
    let y = row + 1;
    for report in [
        format!("^[[<0;3;{y}M"),
        format!("^[[<32;4;{y}M"),
        format!("^[[<0;4;{y}m"),
    ] {
        until(&mut h, &report, |s| s.text().contains(&report));
    }
    assert_eq!(selection(&mut h), None, "the program owns plain drags");
    // Shift bypasses reporting and selects.
    let prompt_row = row_of(&mut h, "$ printf");
    let a = at(&mut h, 2, prompt_row, 0.2);
    let b = at(&mut h, 7, prompt_row, 0.8);
    pause(&mut h);
    modifiers(&mut h, Modifiers::SHIFT);
    drag(&mut h, a, b);
    modifiers(&mut h, Modifiers::NONE);
    assert_eq!(selection(&mut h).as_deref(), Some("printf"));
    assert!(mode(&mut h).contains(TermMode::SGR_MOUSE));
}

#[test]
fn wheel_on_the_alternate_screen_sends_arrow_keys() {
    let d = tempfile::tempdir().unwrap();
    let mut h = run(d.path(), "printf '\\033[?1049h'; cat");
    until(&mut h, "alternate screen", |s| {
        s.term.lock().unwrap().mode().contains(TermMode::ALT_SCREEN)
    });
    let pos = at(&mut h, 10, 10, 0.5);
    move_to(&mut h, pos);
    h.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: Vec2::new(0., 1.),
        modifiers: Modifiers::NONE,
    });
    h.step();
    until(&mut h, "three Up keys", |s| {
        s.text().contains("^[[A^[[A^[[A")
    });
}

#[test]
fn focus_changes_are_reported_when_the_program_asks() {
    let d = tempfile::tempdir().unwrap();
    let mut h = run(d.path(), "printf '\\033[?1004h'; cat");
    until(&mut h, "focus mode", |s| {
        s.term
            .lock()
            .unwrap()
            .mode()
            .contains(TermMode::FOCUS_IN_OUT)
    });
    let set_focus = |h: &mut H<'_>, focused: bool| {
        h.input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .focused = Some(focused);
        h.step();
    };
    set_focus(&mut h, false);
    until(&mut h, "focus out", |s| s.text().contains("^[[O"));
    set_focus(&mut h, true);
    until(&mut h, "focus in", |s| s.text().contains("^[[O^[[I"));
}

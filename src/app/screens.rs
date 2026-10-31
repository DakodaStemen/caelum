//! Offscreen renders of the real UI via `egui_kittest` + wgpu. Ignored by default
//! (they need a GPU or software Vulkan/GL adapter); run with
//! `cargo test --bin caelum-rs screens -- --ignored` and look in `target/screenshots/`.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/screenshots");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn app(ctx: &egui::Context, profile: &Path, state: State) -> App {
    let mut app = App::new(
        ctx,
        profile.to_path_buf(),
        state,
        String::new(),
        None,
        profile.join("config.toml"),
        look(),
        false,
    );
    app.test_shell = true;
    app
}

/// The test machine's look: Adwaita dark with a red accent (like the author's desktop).
fn look() -> SystemLook {
    SystemLook {
        scheme: Some(appearance::Scheme::Dark),
        accent: appearance::named_accent("red"),
        ui_font: Some(("Adwaita Sans".into(), 11.)),
        buttons: appearance::parse_button_layout("appmenu:close"),
    }
}

type H<'a> = Harness<'a, Option<App>>;

/// A harness whose `App` is created with the harness's own context.
fn harness<'a>(size: Vec2, ppp: f32, profile: &Path, state: State) -> H<'a> {
    let mut h = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(ppp)
        .with_max_steps(8)
        .wgpu()
        .build_state(
            |ctx, app: &mut Option<App>| {
                if let Some(app) = app {
                    app.frame(ctx);
                }
            },
            None,
        );
    let ctx = h.ctx.clone();
    *h.state_mut() = Some(app(&ctx, profile, state));
    h
}

fn save(h: &mut H<'_>, name: &str) -> image::RgbaImage {
    // Let the shell print its prompt and background fonts settle.
    for _ in 0..6 {
        h.step();
        std::thread::sleep(Duration::from_millis(60));
    }
    let image = h.render().expect("offscreen render");
    image.save(out_dir().join(format!("{name}.png"))).unwrap();
    image
}

fn crop(image: &image::RgbaImage, region: Rect, ppp: f32, scale: u32) -> image::RgbaImage {
    let (x, y) = ((region.min.x * ppp) as u32, (region.min.y * ppp) as u32);
    let (w, h) = (
        (region.width() * ppp) as u32,
        (region.height() * ppp) as u32,
    );
    let crop = image::imageops::crop_imm(image, x, y, w, h).to_image();
    image::imageops::resize(
        &crop,
        w * scale,
        h * scale,
        image::imageops::FilterType::Nearest,
    )
}
/// accesskit bounds are in physical pixels.
fn node_rect(h: &H<'_>, label: &str, ppp: f32) -> Rect {
    let r = h
        .get_all_by_label(label)
        .next()
        .unwrap_or_else(|| panic!("no widget labelled {label:?}"))
        .rect();
    Rect::from_min_max(
        (r.min.to_vec2() / ppp).to_pos2(),
        (r.max.to_vec2() / ppp).to_pos2(),
    )
}
fn with_light(mut state: State) -> State {
    state.preferences.follow_system = true;
    state
}
fn app_mut<'s>(h: &'s mut H<'_>) -> &'s mut App {
    h.state_mut().as_mut().unwrap()
}

#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn window_dark_and_light() {
    for dark in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let mut h = harness(Vec2::new(1100., 680.), 1., d.path(), State::default());
        if !dark {
            app_mut(&mut h).look.scheme = Some(appearance::Scheme::Light);
        }
        save(&mut h, if dark { "window-dark" } else { "window-light" });
    }
}

#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn single_pane_tabs_and_sidebar() {
    let d = tempfile::tempdir().unwrap();
    let mut state = with_light(State::default());
    // One terminal per tab, three tabs, the second renamed.
    let cwd = PathBuf::from("/tmp");
    let w = &mut state.workspaces[0];
    w.tabs[0].layout = Pane::terminal(cwd.clone());
    for name in ["Build", "Terminal"] {
        let mut t = Tab::new(cwd.clone());
        t.name = name.into();
        w.tabs.push(t);
    }
    state.workspaces.push(Workspace::new("Side project", cwd));
    state.preferences.sidebar = true;
    let mut h = harness(Vec2::new(1000., 560.), 1., d.path(), state);
    save(&mut h, "tabs-sidebar");
}

#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn menus_and_split_popover() {
    let d = tempfile::tempdir().unwrap();
    let mut h = harness(Vec2::new(1000., 640.), 1., d.path(), State::default());
    save(&mut h, "tmp");
    let menu = node_rect(&h, "Main Menu", 1.);
    h.get_all_by_label("Main Menu").next().unwrap().click();
    h.hover_at(menu.center() + Vec2::new(-80., 120.));
    save(&mut h, "main-menu");
    h.key_press(egui::Key::Escape);
    save(&mut h, "tmp");
    h.get_all_by_label("Split").next().unwrap().click();
    let image = save(&mut h, "split-popover");
    let split = node_rect(&h, "Split", 1.);
    let region = Rect::from_min_max(
        Pos2::new(split.right() - 150., split.top() - 6.),
        Pos2::new(split.right() + 20., split.bottom() + 130.),
    );
    crop(&image, region, 1., 3)
        .save(out_dir().join("split-popover-zoom.png"))
        .unwrap();
}

#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn splitting_right_repeatedly_stays_on_screen() {
    let d = tempfile::tempdir().unwrap();
    let mut state = State::default();
    state.workspaces[0].tabs[0].layout = Pane::terminal(PathBuf::from("/tmp"));
    let mut h = harness(Vec2::new(1200., 560.), 1., d.path(), state);
    save(&mut h, "tmp");
    let ctx = h.ctx.clone();
    for _ in 0..12 {
        app_mut(&mut h).action(Action::Split(Direction::Right), &ctx);
        h.step();
    }
    let app = app_mut(&mut h);
    let mut leaves = vec![];
    app.tab().layout.leaves(&mut leaves);
    let columns = leaves.len();
    let message = app.message.clone();
    save(&mut h, "split-right-many");
    assert!(columns >= 5, "{columns}");
    assert!(message.contains("No room"), "{message}");
    // Four directions around a middle pane, then balance.
    let ctx = h.ctx.clone();
    let app = app_mut(&mut h);
    app.tab_mut().layout = Pane::terminal(PathBuf::from("/tmp"));
    app.message.clear();
    app.ensure_focus();
    let centre = app.focused.clone();
    for dir in Direction::ALL {
        app.focused = centre.clone();
        app.action(Action::Split(dir), &ctx);
    }
    save(&mut h, "split-four-directions");
}

/// Crops of the pane header's right edge (close button) at several zoom levels, with
/// and without the pointer over the button.
#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn pane_close_button_zoom_levels() {
    let mut strips = vec![];
    for ppp in [1.0_f32, 1.1, 1.25, 1.5, 1.75, 2.0] {
        for hover in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let mut h = harness(Vec2::new(900., 560.), ppp, d.path(), State::default());
            save(&mut h, "tmp");
            let close = node_rect(&h, "Close Pane", ppp);
            if hover {
                h.hover_at(close.center());
            }
            let image = save(&mut h, "tmp");
            let region = close.expand2(Vec2::new(70., 10.));
            strips.push(crop(&image, region, ppp, (4. / ppp).round() as u32));
        }
    }
    let w = strips.iter().map(|s| s.width()).max().unwrap();
    let h = strips.iter().map(|s| s.height() + 4).sum();
    let mut sheet = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 255, 255]));
    let mut y = 0;
    for s in strips {
        image::imageops::overlay(&mut sheet, &s, 0, y as i64);
        y += s.height() + 4;
    }
    sheet
        .save(out_dir().join("close-x-zoom-sheet.png"))
        .unwrap();
}

#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn csd_headerbar_preferences_and_palette() {
    let d = tempfile::tempdir().unwrap();
    let mut h = harness(Vec2::new(1000., 640.), 1., d.path(), State::default());
    app_mut(&mut h).csd = true;
    app_mut(&mut h).settings = true;
    save(&mut h, "preferences-csd");
    let ctx = h.ctx.clone();
    let app = app_mut(&mut h);
    app.settings = false;
    app.look.scheme = Some(appearance::Scheme::Light);
    app.action(Action::Palette, &ctx);
    save(&mut h, "palette-light-csd");
}

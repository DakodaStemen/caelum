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
    );
    app.test_shell = true;
    app
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

#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn window_default_dark() {
    let d = tempfile::tempdir().unwrap();
    let mut h = harness(Vec2::new(1100., 680.), 1., d.path(), State::default());
    save(&mut h, "window-dark");
}

/// Crops of the pane header's right edge (close button) at several zoom levels, with
/// and without the pointer over the button.
#[test]
#[ignore = "needs a wgpu adapter; writes PNGs to target/screenshots"]
fn pane_close_button_zoom_levels() {
    for ppp in [1.0_f32, 1.1, 1.25, 1.5, 1.75, 2.0] {
        for hover in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let mut h = harness(Vec2::new(900., 560.), ppp, d.path(), State::default());
            save(&mut h, "tmp");
            let close = h
                .get_all_by_label("×")
                .next()
                .map(|n| n.rect())
                .expect("close button");
            // accesskit bounds are in physical pixels.
            let close = Rect::from_min_max(
                (close.min.to_vec2() / ppp).to_pos2(),
                (close.max.to_vec2() / ppp).to_pos2(),
            );
            if hover {
                h.hover_at(close.center());
            }
            let image = save(&mut h, "tmp");
            let region = close.expand2(Vec2::new(70., 14.));
            let (x, y) = ((region.min.x * ppp) as u32, (region.min.y * ppp) as u32);
            let (w, hgt) = (
                (region.width() * ppp) as u32,
                (region.height() * ppp) as u32,
            );
            let crop = image::imageops::crop_imm(&image, x, y, w, hgt).to_image();
            let crop = image::imageops::resize(
                &crop,
                w * 4,
                hgt * 4,
                image::imageops::FilterType::Nearest,
            );
            crop.save(out_dir().join(format!(
                "close-x-{ppp:.2}{}.png",
                if hover { "-hover" } else { "" }
            )))
            .unwrap();
        }
    }
}

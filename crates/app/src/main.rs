//! BASidekick Building Sim: pick a fake building, press Start, and it's on
//! BACnet/IP — a site router plus one device per controller, all running a
//! lite model of the building behind them. Scenarios break things on cue.
//!
//! `basidekick-sim` opens the window; `--package <id> --start` opens it on a
//! running building. `basidekick-sim-cli` is the same thing in a terminal.

// no console window behind the app on Windows (debug builds keep it for logs)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod brand;
mod chart;
mod ui;

use gpui_kit::{
    App, AppContext, AssetSource, Bounds, KeyBinding, Menu, MenuItem, SharedString,
    TitlebarOptions, WindowBounds, WindowOptions, actions, px, size,
};
use std::borrow::Cow;

gpui_kit::assets::icon_assets!(
    SimIcons,
    [
        Play,
        Pause,
        Square,
        Building2,
        School,
        Store,
        Warehouse,
        Thermometer,
        Fan,
        Snowflake,
        Flame,
        Sun,
        Users,
        Zap,
        WifiOff,
        Siren,
        Wind,
        Gauge,
        Lightbulb,
        Activity,
        Clock,
        Network,
        TriangleAlert,
        DoorOpen,
        Hand,
        ChevronDown,
        ChevronRight,
        Eraser,
        Router,
        Droplets,
        FolderOpen,
        CloudSun,
        Dices
    ]
);

struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if path == brand::TRACE_S {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../../assets/brand/trace-s.svg"
            ))));
        }
        if let Some(bytes) = SimIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(SimIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

actions!(basidekick_sim, [Quit]);

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("-psn_"))
        .collect();
    // a terminal run on macOS and Linux; Windows has basidekick-sim-cli for that
    #[cfg(not(windows))]
    {
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!(
                "basidekick-sim [--package <id>] [--start] [--lesson <id>]   open the app (on a building, started, set up for a lesson)\n"
            );
            print!("{}", bask_runner::cli::usage());
            return;
        }
        if args.iter().any(|a| a == "--headless" || a == "--list") {
            if let Err(e) = bask_runner::cli::run(&args) {
                eprintln!("basidekick-sim: {e}");
                std::process::exit(1);
            }
            return;
        }
    }

    let package = args
        .iter()
        .position(|a| a == "--package")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let start = args.iter().any(|a| a == "--start");
    let lesson = args
        .iter()
        .position(|a| a == "--lesson")
        .and_then(|i| args.get(i + 1))
        .cloned();
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            brand::fonts(cx);
            brand::apply_theme(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            // cmd-q on macOS, ctrl-q elsewhere
            cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
            cx.set_menus(vec![Menu {
                name: "BASidekick Building Sim".into(),
                items: vec![MenuItem::action("Quit BASidekick Building Sim", Quit)],
                disabled: false,
            }]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1440.), px(920.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1180.), px(720.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("BASidekick Building Sim".into()),
                        ..Default::default()
                    }),
                    // matches the Linux .desktop file
                    app_id: Some("com.basidekick.buildingsim".into()),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        ui::SimApp::new(package.as_deref(), start, lesson.clone(), window, cx)
                    })
                },
            )
            .unwrap();
            cx.activate(true);
        });
}

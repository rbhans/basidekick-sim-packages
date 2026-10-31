//! The window: buildings and network settings on the left, the running
//! building in the middle (clock, zones, controllers and their points), the
//! scenarios and the event log on the right.

use crate::brand::{self, chip, tint};
use bask_bacnet::NetInterface;
use bask_runner::runtime::{NetConfig, Runtime};
use bask_sim::package::EquipDef;
use bask_sim::{
    Category, DeviceSnap, Kind, Level, Package, ScenarioDef, ScenarioSnap, Season, Snapshot,
    ZoneSnap,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{Disableable, Icon, Selectable, Sizable};
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, ElementId, Entity, FontWeight, Hsla, ScrollHandle,
    SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::Duration;

const SPEEDS: [f64; 4] = [1.0, 10.0, 60.0, 300.0];
const JUMPS: [(&str, f64, bool); 5] = [
    ("6 AM", 6.0, true),
    ("8 AM", 8.0, true),
    ("Noon", 12.0, true),
    ("5 PM", 17.0, true),
    ("10 PM", 22.0, false),
];

pub struct SimApp {
    packages: Vec<Package>,
    package_errors: Vec<String>,
    selected: usize,
    interfaces: Vec<NetInterface>,
    iface: usize,
    port: Entity<InputState>,
    runtime: Option<Runtime>,
    snap: Option<Snapshot>,
    expanded: HashSet<usize>,
    quick: HashMap<usize, Vec<ScenarioDef>>,
    /// Mystery faults whose answer is showing.
    revealed: HashSet<String>,
    error: Option<String>,
    log_scroll: ScrollHandle,
    last_seq: u64,
    _poll: Task<()>,
    _quit: Subscription,
}

impl SimApp {
    /// `package` preselects a building by id; `start` starts it right away.
    pub fn new(
        package: Option<&str>,
        start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (packages, package_errors) = Package::discover(Some(&bask_runner::user_packages_dir()));
        let selected = package
            .and_then(|id| packages.iter().position(|p| p.id == id))
            .unwrap_or(0);
        let poll = cx.spawn(async move |this, cx| {
            for tick in 0u64.. {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this.update(cx, |this, cx| this.tick(tick, cx)).is_err() {
                    break;
                }
            }
        });
        let quit = cx.on_app_quit(|this, _| {
            if let Some(rt) = this.runtime.take() {
                rt.stop();
            }
            async {}
        });
        let mut app = SimApp {
            packages,
            package_errors,
            selected,
            interfaces: bask_bacnet::interfaces(),
            iface: 0,
            port: cx.new(|cx| InputState::new(window, cx).default_value("47808")),
            runtime: None,
            snap: None,
            expanded: HashSet::new(),
            quick: HashMap::new(),
            revealed: HashSet::new(),
            error: None,
            log_scroll: ScrollHandle::new(),
            last_seq: 0,
            _poll: poll,
            _quit: quit,
        };
        if start {
            app.start(cx);
        }
        app
    }

    fn tick(&mut self, tick: u64, cx: &mut Context<Self>) {
        match &self.runtime {
            Some(rt) => {
                let snap = rt.sim.lock().snapshot();
                let newest = snap.log.last().map_or(0, |e| e.seq);
                if newest != self.last_seq {
                    // keep following the log unless scrolled up to read
                    let (offset, max) = (self.log_scroll.offset(), self.log_scroll.max_offset());
                    if max.y + offset.y < px(24.) {
                        self.log_scroll.scroll_to_bottom();
                    }
                    self.last_seq = newest;
                }
                self.snap = Some(snap);
                cx.notify();
            }
            None if tick.is_multiple_of(20) => {
                let fresh = bask_bacnet::interfaces();
                if fresh
                    .iter()
                    .map(|i| i.ip)
                    .ne(self.interfaces.iter().map(|i| i.ip))
                {
                    let current = self.interfaces.get(self.iface).map(|i| i.ip);
                    self.iface = fresh
                        .iter()
                        .position(|i| Some(i.ip) == current)
                        .unwrap_or(0);
                    self.interfaces = fresh;
                    cx.notify();
                }
            }
            None => {}
        }
    }

    fn net_config(&self, cx: &Context<Self>) -> Result<NetConfig, String> {
        let text = self.port.read(cx).value();
        let port: u16 = text
            .trim()
            .parse()
            .map_err(|_| format!("{text:?} isn't a UDP port"))?;
        Ok(match self.interfaces.get(self.iface) {
            Some(i) => NetConfig {
                ip: i.ip,
                port,
                broadcast: Some(i.broadcast),
            },
            None => NetConfig {
                ip: Ipv4Addr::LOCALHOST,
                port,
                broadcast: None,
            },
        })
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        let net = match self.net_config(cx) {
            Ok(n) => n,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        match Runtime::start(self.packages[self.selected].clone(), net) {
            Ok(rt) => {
                self.snap = Some(rt.sim.lock().snapshot());
                self.runtime = Some(rt);
                self.expanded.clear();
                self.quick.clear();
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(rt) = self.runtime.take() {
            rt.stop();
        }
        self.snap = None;
        cx.notify();
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix == self.selected {
            return;
        }
        self.selected = ix;
        if self.runtime.is_some() {
            self.stop(cx);
            self.start(cx);
        }
        cx.notify();
    }

    fn with_sim(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut bask_sim::Simulation)) {
        if let Some(rt) = &self.runtime {
            f(&mut rt.sim.lock());
            self.snap = Some(rt.sim.lock().snapshot());
        }
        cx.notify();
    }

    fn toggle_device(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&ix) {
            self.expanded.insert(ix);
            if let Some(rt) = &self.runtime {
                let faults = rt.sim.lock().quick_faults(ix);
                self.quick.insert(ix, faults);
            }
        }
        cx.notify();
    }

    // ---- header -----------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pkg = &self.packages[self.selected];
        let (dot, status) = match (&self.runtime, &self.snap) {
            (Some(rt), Some(snap)) => {
                let n = snap.devices.len();
                (
                    brand::accent(),
                    format!(
                        "Running {} · {n} BACnet devices on {} · network {}",
                        snap.building, rt.local, snap.network
                    ),
                )
            }
            _ => (brand::ink_3(), format!("Stopped · {} ready", pkg.name)),
        };
        let button = if self.runtime.is_some() {
            Button::new("run")
                .danger()
                .icon(IconName::Square)
                .label("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        } else {
            Button::new("run")
                .primary()
                .icon(IconName::Play)
                .label("Start building")
                .on_click(cx.listener(|this, _, _, cx| this.start(cx)))
        };
        let traffic = self
            .runtime
            .as_ref()
            .and_then(|rt| rt.last_client())
            .map(|(addr, at)| {
                let ago = at.elapsed().as_secs();
                let fresh = ago < 10;
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(if fresh {
                        brand::ink_2()
                    } else {
                        brand::ink_3()
                    })
                    .child(div().size(px(6.)).rounded_full().bg(if fresh {
                        brand::ok()
                    } else {
                        brand::ink_3()
                    }))
                    .child(format!(
                        "{} · {} requests",
                        addr.ip(),
                        self.runtime.as_ref().map_or(0, |r| r.requests())
                    ))
            });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .h(px(60.))
            .px_5()
            .border_b_1()
            .border_color(brand::line())
            .bg(brand::well())
            .child(brand::wordmark(20.))
            .child(
                div()
                    .font_family(brand::MONO_FONT)
                    .text_xs()
                    .text_color(brand::ink_3())
                    .pl_3()
                    .border_l_1()
                    .border_color(brand::line_2())
                    .child("BUILDING SIM"),
            )
            .child(div().flex_1())
            .children(traffic)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(brand::ink_2())
                    .child(div().size(px(8.)).rounded_full().bg(dot))
                    .child(status),
            )
            .child(button)
    }

    // ---- left sidebar -------------------------------------------------------

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.runtime.is_some();
        let buildings = self.packages.iter().enumerate().map(|(ix, p)| {
            let on = ix == self.selected;
            div()
                .id(("pkg", ix))
                .cursor_pointer()
                .flex()
                .gap_3()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(if on { brand::accent() } else { brand::line() })
                .bg(if on { brand::panel_2() } else { brand::panel() })
                .hover(|s| s.bg(brand::panel_2()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(ix, cx)))
                .child(
                    div()
                        .flex_none()
                        .size(px(32.))
                        .rounded_md()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if on { brand::accent() } else { brand::well() })
                        .text_color(if on { brand::ink() } else { brand::ink_2() })
                        .child(Icon::new(package_icon(&p.icon)).size_4()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(p.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(brand::ink_3())
                                .child(p.tagline.clone()),
                        ),
                )
        });

        let ifaces = self.interfaces.iter().enumerate().map(|(ix, i)| {
            let on = ix == self.iface;
            let label = if i.ip.is_loopback() {
                "Loopback (this computer only)".to_string()
            } else {
                i.name.clone()
            };
            div()
                .id(("iface", ix))
                .when(!running, |d| {
                    d.cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.iface = ix;
                            cx.notify();
                        }))
                })
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(if on { brand::panel_2() } else { brand::panel() })
                .border_1()
                .border_color(if on { brand::line_2() } else { brand::line() })
                .opacity(if running && !on { 0.4 } else { 1.0 })
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .border_1()
                        .border_color(brand::ink_3())
                        .when(on, |d| d.bg(brand::accent())),
                )
                .child(div().flex_1().text_sm().child(label))
                .child(
                    div()
                        .font_family(brand::MONO_FONT)
                        .text_xs()
                        .text_color(brand::ink_3())
                        .child(i.ip.to_string()),
                )
        });

        let pkg = &self.packages[self.selected];
        div()
            .id("sidebar")
            .flex_none()
            .w(px(280.))
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .border_r_1()
            .border_color(brand::line())
            .child(section_title("Buildings"))
            .children(buildings)
            .child(
                Button::new("open-packages")
                    .ghost()
                    .xsmall()
                    .icon(IconName::FolderOpen)
                    .label("Your own packages…")
                    .tooltip("Drop .toml building packages in this folder; they show up next launch")
                    .on_click(|_, _, _| {
                        let _ = bask_runner::open_folder(&bask_runner::user_packages_dir());
                    }),
            )
            .children(self.package_errors.iter().map(|e| div().text_xs().text_color(brand::fault()).child(e.clone())))
            .child(div().h(px(12.)))
            .child(section_title("BACnet/IP"))
            .children(ifaces)
            .when(self.interfaces.is_empty(), |d| {
                d.child(div().text_xs().text_color(brand::fault()).child("No network interface found; serving on 127.0.0.1"))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mt_1()
                    .child(div().text_sm().text_color(brand::ink_2()).w(px(60.)).child("UDP port"))
                    .child(div().flex_1().child(Input::new(&self.port).small().disabled(running))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(brand::ink_3())
                    .mt_1()
                    .child(format!(
                        "Site router is device {}; controllers {}–{} sit behind it on network {}.",
                        pkg.site_instance,
                        pkg.site_instance + 1,
                        pkg.site_instance + pkg.equipment.len() as u32,
                        pkg.network
                    )),
            )
            .when(running, |d| d.child(div().text_xs().text_color(brand::ink_3()).child("Stop the building to change these.")))
            .when(!running, |d| {
                d.child(div().text_xs().text_color(brand::ink_3()).child(
                    "A BAS on this computer already holds 47808? Use 47809 here and add a second BACnet/IP network for it.",
                ))
            })
    }

    // ---- stopped: the package overview --------------------------------------

    fn overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pkg = &self.packages[self.selected];
        let controllers = pkg.equipment.iter().enumerate().map(|(i, e)| {
            let c = e.common();
            let instance = c.instance.unwrap_or(pkg.site_instance + i as u32 + 1);
            table_row(vec![
                (mono(c.id.clone()).w(px(120.)), false),
                (
                    div()
                        .w(px(150.))
                        .text_color(brand::ink_2())
                        .child(equip_label(e)),
                    false,
                ),
                (
                    mono(format!("{instance}"))
                        .w(px(80.))
                        .text_color(brand::ink_2()),
                    false,
                ),
                (
                    mono(format!("{}:{}", pkg.network, i + 1))
                        .w(px(90.))
                        .text_color(brand::ink_3()),
                    false,
                ),
                (
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_color(brand::ink_3())
                        .truncate()
                        .child(c.description.clone()),
                    true,
                ),
            ])
        });
        let zones = pkg.zones.iter().map(|z| {
            table_row(vec![
                (div().w(px(200.)).child(z.name.clone()), false),
                (
                    mono(format!("{:.0} sq ft", z.area_sqft))
                        .w(px(110.))
                        .text_color(brand::ink_2()),
                    false,
                ),
                (
                    mono(format!("{:.0} people", z.people))
                        .w(px(110.))
                        .text_color(brand::ink_2()),
                    false,
                ),
                (
                    div()
                        .flex_1()
                        .text_color(brand::ink_3())
                        .child(format!("faces {}", z.facing)),
                    true,
                ),
            ])
        });
        let scenario_count = pkg.scenarios.len() + bask_sim::scenario::global_scenarios().len();
        div()
            .id("overview")
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_y_scroll()
            .p_8()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .font_family(brand::DISPLAY_FONT)
                    .text_size(px(34.))
                    .child(pkg.name.clone()),
            )
            .child(
                div()
                    .text_color(brand::ink_2())
                    .max_w(px(760.))
                    .child(pkg.description.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(stat_chip(format!("{:.0} sq ft", pkg.area_sqft)))
                    .child(stat_chip(format!("{} zones", pkg.zones.len())))
                    .child(stat_chip(format!(
                        "{} BACnet devices",
                        pkg.equipment.len() + 1
                    )))
                    .child(stat_chip(format!("{scenario_count} scenarios")))
                    .when(!pkg.location.is_empty(), |d| {
                        d.child(stat_chip(pkg.location.clone()))
                    }),
            )
            .child(
                div().flex().items_center().gap_3().mt_2().child(
                    Button::new("start-big")
                        .primary()
                        .large()
                        .icon(IconName::Play)
                        .label("Start building")
                        .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                ),
            )
            .children(self.error.clone().map(|e| {
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(tint(brand::alarm(), 0.12))
                    .border_1()
                    .border_color(brand::alarm())
                    .text_sm()
                    .child(e)
            }))
            .child(div().h(px(8.)))
            .child(section_title("Controllers"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(brand::line())
                    .overflow_hidden()
                    .child(table_row(vec![
                        (mono(pkg.site_name.clone()).w(px(120.)), false),
                        (
                            div()
                                .w(px(150.))
                                .text_color(brand::ink_2())
                                .child("Site router"),
                            false,
                        ),
                        (
                            mono(format!("{}", pkg.site_instance))
                                .w(px(80.))
                                .text_color(brand::ink_2()),
                            false,
                        ),
                        (
                            mono("BACnet/IP").w(px(90.)).text_color(brand::ink_3()),
                            false,
                        ),
                        (
                            div()
                                .flex_1()
                                .text_color(brand::ink_3())
                                .child("Weather station, building occupancy, fire alarm"),
                            true,
                        ),
                    ]))
                    .children(controllers),
            )
            .child(div().h(px(8.)))
            .child(section_title("Zones"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(brand::line())
                    .overflow_hidden()
                    .children(zones),
            )
    }

    // ---- running: clock, zones, controllers ---------------------------------

    fn clock_bar(&self, snap: &Snapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let occ = if snap.occupied {
            chip("OCCUPIED", brand::ink(), brand::accent())
        } else {
            chip("UNOCCUPIED", brand::ink_2(), brand::panel_2())
        };
        let hours = match snap.hours {
            Some((open, close)) => format!("open {}–{}", hhmm(open), hhmm(close)),
            None => "closed today".into(),
        };
        let speeds = SPEEDS.iter().enumerate().map(|(i, &s)| {
            segment(
                ("speed", i),
                &format!("{s:.0}×"),
                !snap.paused && snap.speed == s,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.with_sim(cx, |sim| {
                    sim.speed = s;
                    sim.paused = false;
                })
            }))
        });
        let seasons = Season::ALL.iter().enumerate().map(|(i, &season)| {
            segment(("season", i), season.label(), snap.season == season).on_click(
                cx.listener(move |this, _, _, cx| this.with_sim(cx, |sim| sim.set_season(season))),
            )
        });
        let jumps = JUMPS
            .iter()
            .enumerate()
            .map(|(i, &(label, hour, weekday))| {
                Button::new(("jump", i))
                    .ghost()
                    .xsmall()
                    .label(label)
                    .tooltip(if weekday {
                        "Run ahead to the next weekday at this time"
                    } else {
                        "Run ahead to this time"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.with_sim(cx, |sim| sim.jump_to(hour, weekday))
                    }))
            });
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap_3()
            .px_5()
            .py_4()
            .border_b_1()
            .border_color(brand::line())
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(brand::ink_3())
                                    .child(snap.now.format("%A, %B %-d").to_string()),
                            )
                            .child(
                                div()
                                    .font_family(brand::MONO_FONT)
                                    .text_size(px(30.))
                                    .line_height(px(34.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(snap.now.format("%-I:%M:%S %p").to_string()),
                            ),
                    )
                    .child(stat(
                        "Outside",
                        format!("{:.1}°F", snap.weather.oat),
                        format!("{:.0} % RH", snap.weather.rh),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(occ)
                            .child(div().text_xs().text_color(brand::ink_3()).child(hours)),
                    )
                    .child(stat(
                        "People",
                        format!("{:.0}", snap.people),
                        "in the building".into(),
                    ))
                    .child(stat(
                        "Demand",
                        format!("{:.1} kW", snap.kw),
                        format!("{:.0} kWh", snap.kwh),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(label("Speed"))
                            .child(
                                segments()
                                    .child(
                                        Button::new("pause")
                                            .small()
                                            .icon(if snap.paused {
                                                IconName::Play
                                            } else {
                                                IconName::Pause
                                            })
                                            .tooltip(if snap.paused {
                                                "Resume"
                                            } else {
                                                "Pause the clock (BACnet keeps answering)"
                                            })
                                            .selected(snap.paused)
                                            .ghost()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.with_sim(cx, |sim| sim.paused = !sim.paused)
                                            })),
                                    )
                                    .children(speeds),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(label("Weather"))
                            .child(segments().children(seasons)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(label("Jump to"))
                            .children(jumps),
                    ),
            )
    }

    fn zone_card(z: &ZoneSnap) -> impl IntoElement {
        let off = z.off_band();
        let color = if off < -0.5 {
            brand::cold()
        } else if off > 0.5 {
            brand::warm()
        } else {
            brand::paper()
        };
        let co2_color = if z.co2 > 1500.0 {
            brand::alarm()
        } else if z.co2 > 1000.0 {
            brand::fault()
        } else {
            brand::ink_2()
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(if off.abs() > 2.0 {
                tint(color, 0.6)
            } else {
                brand::line()
            })
            .bg(brand::panel())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(z.name.clone()),
                    )
                    .when(z.door_open, |d| {
                        d.child(
                            div()
                                .text_color(brand::fault())
                                .child(Icon::new(IconName::DoorOpen).size_3p5()),
                        )
                    })
                    .child(
                        div()
                            .text_color(if z.lights_on {
                                brand::accent()
                            } else {
                                brand::ink_3()
                            })
                            .child(Icon::new(IconName::Lightbulb).size_3p5()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .child(
                        div()
                            .font_family(brand::MONO_FONT)
                            .text_size(px(26.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(color)
                            .child(format!("{:.1}°", z.temp)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(brand::ink_3())
                            .child(z.band_text()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_xs()
                    .font_family(brand::MONO_FONT)
                    .child(
                        div()
                            .text_color(co2_color)
                            .child(format!("{:.0} ppm", z.co2)),
                    )
                    .child(
                        div()
                            .text_color(brand::ink_2())
                            .child(format!("{:.0} ppl", z.people)),
                    )
                    .child(
                        div()
                            .text_color(brand::ink_3())
                            .child(if z.supply_cfm > 5.0 {
                                format!("{:.0} cfm @ {:.0}°", z.supply_cfm, z.supply_temp)
                            } else {
                                "no air".into()
                            }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(brand::ink_3())
                    .truncate()
                    .child(z.served_by.clone()),
            )
    }

    fn device_row(
        &self,
        d: &DeviceSnap,
        cx: &mut Context<Self>,
        active: &[ScenarioSnap],
    ) -> AnyElement {
        let open = self.expanded.contains(&d.index);
        let ix = d.index;
        let addr = match d.mac {
            Some(mac) => format!(
                "{} · {}:{mac}",
                d.instance,
                self.snap.as_ref().map_or(0, |s| s.network)
            ),
            None => format!("{} · router", d.instance),
        };
        let mut badges: Vec<Div> = Vec::new();
        if !d.online {
            badges.push(chip("OFFLINE", brand::paper(), tint(brand::ink_3(), 0.35)));
        }
        if d.alarms > 0 {
            badges.push(chip(
                format!("{} ALARM", d.alarms),
                brand::paper(),
                brand::alarm(),
            ));
        }
        if d.faults > 0 {
            badges.push(chip(
                format!("{} FAULT", d.faults),
                brand::ink(),
                brand::fault(),
            ));
        }
        if d.overrides > 0 {
            badges.push(chip(
                format!("{} HAND", d.overrides),
                brand::paper(),
                brand::hand(),
            ));
        }
        let head = div()
            .id(("dev", ix))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_device(ix, cx)))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .hover(|s| s.bg(brand::panel_2()))
            .child(
                div()
                    .flex_none()
                    .size(px(28.))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(brand::well())
                    .text_color(if d.online {
                        brand::ink_2()
                    } else {
                        brand::ink_3()
                    })
                    .child(Icon::new(kind_icon(d.kind)).size_4()),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(150.))
                    .flex()
                    .flex_col()
                    .child(mono(d.name.clone()).font_weight(FontWeight::BOLD))
                    .child(
                        div()
                            .text_xs()
                            .text_color(brand::ink_3())
                            .child(d.kind.label()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .text_color(brand::ink_2())
                    .child(d.headline.clone()),
            )
            .children(badges)
            .child(mono(addr).flex_none().text_xs().text_color(brand::ink_3()))
            .child(
                div().text_color(brand::ink_3()).child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size_4(),
                ),
            );
        let mut row = div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(brand::line())
            .when(!d.online, |r| r.opacity(0.6))
            .child(head);
        if open {
            let points =
                d.points.iter().enumerate().map(|(i, p)| {
                    let mut flags: Vec<Div> = Vec::new();
                    if let Some(pr) = p.priority {
                        flags.push(chip(format!("P{pr}"), brand::paper(), brand::hand()));
                    }
                    if p.out_of_service {
                        flags.push(chip("OOS", brand::paper(), brand::hand()));
                    }
                    if p.fault {
                        flags.push(chip("FAULT", brand::ink(), brand::fault()));
                    }
                    if p.alarm {
                        flags.push(chip("ALARM", brand::paper(), brand::alarm()));
                    }
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .when(i % 2 == 1, |r| r.bg(tint(brand::well(), 0.5)))
                        .child(mono(p.name.clone()).w(px(130.)).flex_none())
                        .child(
                            mono(format!("{} {}", p.kind, p.instance))
                                .w(px(64.))
                                .flex_none()
                                .text_xs()
                                .text_color(brand::ink_3()),
                        )
                        .child(mono(p.value.clone()).w(px(120.)).flex_none().text_color(
                            if p.alarm {
                                brand::alarm()
                            } else if p.priority.is_some() {
                                hand_text()
                            } else {
                                brand::paper()
                            },
                        ))
                        .child(div().flex().gap_1().w(px(110.)).flex_none().children(flags))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_xs()
                                .text_color(brand::ink_3())
                                .child(p.description.clone()),
                        )
                });
            let faults = self.quick.get(&ix).map(|faults| {
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(label("Break it"))
                    .children(faults.iter().enumerate().map(|(fi, f)| {
                        let on = active.iter().any(|a| a.id == f.id && !a.ending);
                        let def = f.clone();
                        let short = f
                            .name
                            .strip_prefix(&format!("{} ", d.name))
                            .unwrap_or(&f.name)
                            .to_string();
                        let b = Button::new(ElementId::Name(format!("qf-{ix}-{fi}").into()))
                            .xsmall()
                            .label(short)
                            .tooltip(f.description.clone());
                        if on {
                            b.danger().icon(IconName::Square).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let id = def.id.clone();
                                    this.with_sim(cx, |sim| sim.stop_scenario(&id))
                                },
                            ))
                        } else {
                            b.outline().icon(IconName::Zap).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let def = def.clone();
                                    this.with_sim(cx, |sim| sim.start(def))
                                },
                            ))
                        }
                    }))
            });
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .bg(brand::well())
                    .border_t_1()
                    .border_color(brand::line())
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(brand::ink_3())
                            .child(d.description.clone()),
                    )
                    .children(points)
                    .children(faults),
            );
        }
        row.into_any_element()
    }

    fn dashboard(&self, snap: &Snapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let zones = snap.zones.iter().map(Self::zone_card);
        let alarms: usize = snap.devices.iter().map(|d| d.alarms + d.faults).sum();
        let active: Vec<ScenarioSnap> = snap
            .scenarios
            .iter()
            .filter(|s| s.active)
            .cloned()
            .collect();
        let devices: Vec<AnyElement> = snap
            .devices
            .iter()
            .map(|d| self.device_row(d, cx, &active))
            .collect();
        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .child(self.clock_bar(snap, cx))
            .child(
                div()
                    .id("dashboard")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(section_title("Zones"))
                    .child(div().grid().grid_cols(3).gap_3().children(zones))
                    .child(div().h(px(10.)))
                    .child(
                        section_title("Controllers").child(div().text_xs().text_color(brand::ink_3()).child(if alarms > 0 {
                            format!("{alarms} points in alarm or fault · click a controller for its points")
                        } else {
                            "click a controller for its points and faults to throw at it".into()
                        })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .rounded_lg()
                            .border_1()
                            .border_color(brand::line())
                            .bg(brand::panel())
                            .overflow_hidden()
                            .children(devices),
                    ),
            )
    }

    // ---- right: scenarios and the log ---------------------------------------

    fn scenario_card(
        &self,
        s: &ScenarioSnap,
        running: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = s.id.clone();
        let button = if s.active && !s.ending {
            Button::new(ElementId::Name(format!("sc-{id}").into()))
                .outline()
                .xsmall()
                .icon(IconName::Square)
                .label("Stop")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = id.clone();
                    this.with_sim(cx, |sim| sim.stop_scenario(&id))
                }))
        } else {
            Button::new(ElementId::Name(format!("sc-{id}").into()))
                .primary()
                .xsmall()
                .icon(IconName::Play)
                .label("Run")
                .disabled(!running || s.ending)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = id.clone();
                    this.with_sim(cx, |sim| {
                        let _ = sim.start_scenario(&id);
                    })
                }))
        };
        let status = s.active.then(|| {
            let text = if s.ending {
                "fading out".to_string()
            } else {
                match s.remaining_s {
                    Some(r) if r >= 3600.0 => format!(
                        "{:.0} h {:.0} min left",
                        (r / 3600.0).floor(),
                        (r % 3600.0) / 60.0
                    ),
                    Some(r) => format!("{:.0} min left", (r / 60.0).ceil()),
                    None => "until stopped".to_string(),
                }
            };
            let progress = match (s.remaining_s, s.duration_min) {
                (Some(r), Some(d)) if d > 0.0 => (1.0 - r / (d * 60.0)) * 100.0,
                _ => s.scale * 100.0,
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div().flex_1().child(
                        Progress::new(ElementId::Name(format!("pr-{}", s.id).into()))
                            .value(progress as f32),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(brand::MONO_FONT)
                        .text_color(brand::accent())
                        .child(text),
                )
        });
        let answer = s.answer.clone().map(|answer| {
            let shown = self.revealed.contains(&s.id);
            let id = s.id.clone();
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Button::new(ElementId::Name(format!("reveal-{}", s.id).into()))
                        .ghost()
                        .xsmall()
                        .label(if shown {
                            "Hide answer"
                        } else {
                            "Reveal answer"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.revealed.remove(&id) {
                                this.revealed.insert(id.clone());
                            }
                            cx.notify();
                        })),
                )
                .when(shown, |d| {
                    d.child(div().text_xs().text_color(brand::accent()).child(answer))
                })
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(if s.active {
                brand::accent()
            } else {
                brand::line()
            })
            .bg(if s.active {
                tint(brand::accent(), 0.06)
            } else {
                brand::panel()
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(s.name.clone()),
                    )
                    .child(button),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(brand::ink_3())
                    .child(s.description.clone()),
            )
            .children(status)
            .children(answer)
    }

    fn right_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.runtime.is_some();
        // stopped: preview the catalog the package will have
        let preview: Vec<ScenarioSnap>;
        let scenarios: &[ScenarioSnap] = match &self.snap {
            Some(s) => &s.scenarios,
            None => {
                let pkg = &self.packages[self.selected];
                let mut defs = bask_sim::scenario::global_scenarios();
                defs.extend(pkg.scenarios.iter().cloned());
                preview = defs
                    .into_iter()
                    .map(|d| ScenarioSnap {
                        id: d.id,
                        name: d.name,
                        description: d.description,
                        category: d.category,
                        duration_min: d.duration_min,
                        active: false,
                        ending: false,
                        scale: 0.0,
                        remaining_s: None,
                        quick: false,
                        answer: None,
                    })
                    .collect();
                &preview
            }
        };
        let any_active = scenarios.iter().any(|s| s.active && !s.ending);
        let quick: Vec<&ScenarioSnap> = scenarios.iter().filter(|s| s.quick).collect();
        let mystery = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(brand::line_2())
            .bg(brand::panel())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).child("Mystery fault"))
                    .child(
                        Button::new("mystery")
                            .primary()
                            .xsmall()
                            .icon(IconName::Dices)
                            .label("Break something")
                            .disabled(!running)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.with_sim(cx, |sim| {
                                    sim.start_mystery();
                                })
                            })),
                    ),
            )
            .child(div().text_xs().text_color(brand::ink_3()).child(
                "Picks a real fault at random and doesn't say which. Find it from the BAS, then reveal the answer.",
            ));
        let mut list = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(category_title("Training"))
            .child(mystery);
        if !quick.is_empty() {
            list = list.child(category_title("Faults you threw"));
            for s in quick {
                list = list.child(self.scenario_card(s, running, cx));
            }
        }
        for cat in Category::ALL {
            let in_cat: Vec<&ScenarioSnap> = scenarios
                .iter()
                .filter(|s| !s.quick && s.category == cat)
                .collect();
            if in_cat.is_empty() {
                continue;
            }
            list = list.child(category_title(cat.label()));
            for s in in_cat {
                list = list.child(self.scenario_card(s, running, cx));
            }
        }

        let log = self.snap.as_ref().map(|snap| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .h(px(260.))
                .border_t_1()
                .border_color(brand::line())
                .bg(brand::well())
                .child(
                    div().flex().items_center().px_4().py_2().child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(brand::ink_3())
                            .child("EVENT LOG"),
                    ),
                )
                .child(
                    div()
                        .id("log")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .track_scroll(&self.log_scroll)
                        .px_4()
                        .pb_2()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .font_family(brand::MONO_FONT)
                        .text_xs()
                        .children(snap.log.iter().map(|e| {
                            let color = match e.level {
                                Level::Alarm => brand::alarm(),
                                Level::Clear => brand::ok(),
                                Level::Scenario => brand::accent(),
                                Level::Bacnet => hand_text(),
                                Level::Info => brand::ink_2(),
                            };
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(brand::ink_3())
                                        .child(e.at.format("%H:%M:%S").to_string()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_color(color)
                                        .child(e.text.clone()),
                                )
                        })),
                )
        });

        div()
            .flex_none()
            .w(px(360.))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(brand::line())
            .child(
                div()
                    .id("scenarios")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        section_title("Scenarios")
                            .when(running, |d| {
                                let n = scenarios.iter().filter(|s| s.active && !s.ending).count();
                                d.child(if n == 0 {
                                    chip("ALL NORMAL", brand::ink(), brand::ok())
                                } else {
                                    chip(format!("{n} RUNNING"), brand::ink(), brand::accent())
                                })
                            })
                            .child(
                                Button::new("stop-all")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Eraser)
                                    .label("Stop all")
                                    .disabled(!any_active)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.with_sim(cx, |sim| sim.stop_all())
                                    })),
                            ),
                    )
                    .when(!running, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(brand::ink_3())
                                .child("Start the building to run these."),
                        )
                    })
                    .child(list),
            )
            .children(log)
    }
}

impl Render for SimApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let center: AnyElement = match self.snap.clone() {
            Some(snap) if self.runtime.is_some() => self.dashboard(&snap, cx).into_any_element(),
            _ => self.overview(cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(brand::ink())
            .text_color(brand::paper())
            .child(self.header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.sidebar(cx))
                    .child(center)
                    .child(self.right_panel(cx)),
            )
    }
}

// ---- small pieces -------------------------------------------------------------

fn package_icon(icon: &str) -> IconName {
    match icon {
        "school" => IconName::School,
        "store" => IconName::Store,
        "warehouse" => IconName::Warehouse,
        _ => IconName::Building2,
    }
}

fn kind_icon(kind: Kind) -> IconName {
    match kind {
        Kind::Site => IconName::Router,
        Kind::Rtu | Kind::Ahu => IconName::Fan,
        Kind::Vav => IconName::Wind,
        Kind::Chiller => IconName::Snowflake,
        Kind::Boiler | Kind::UnitHeater => IconName::Flame,
        Kind::ExhaustFan => IconName::Fan,
        Kind::Lighting => IconName::Lightbulb,
        Kind::Meter => IconName::Gauge,
    }
}

fn equip_label(e: &EquipDef) -> &'static str {
    match e {
        EquipDef::Rtu(_) => "Rooftop unit",
        EquipDef::Ahu(_) => "Air handler",
        EquipDef::Vav(_) => "VAV box",
        EquipDef::ChillerPlant(_) => "Chiller plant",
        EquipDef::BoilerPlant(_) => "Boiler plant",
        EquipDef::ExhaustFan(_) => "Exhaust fan",
        EquipDef::Lighting(_) => "Lighting panel",
        EquipDef::Meter(_) => "Electric meter",
        EquipDef::UnitHeater(_) => "Unit heater",
    }
}

/// The hand blue lifted enough to read as text on ink.
fn hand_text() -> Hsla {
    Hsla {
        l: 0.72,
        ..brand::hand()
    }
}

fn hhmm(h: f64) -> String {
    let (hh, mm) = (h.floor() as u32, ((h - h.floor()) * 60.0).round() as u32);
    let (h12, ampm) = match hh {
        0 => (12, "AM"),
        1..=11 => (hh, "AM"),
        12 => (12, "PM"),
        _ => (hh - 12, "PM"),
    };
    if mm == 0 {
        format!("{h12} {ampm}")
    } else {
        format!("{h12}:{mm:02} {ampm}")
    }
}

fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(brand::MONO_FONT).child(text.into())
}

fn label(text: &'static str) -> Div {
    div().text_xs().text_color(brand::ink_3()).child(text)
}

fn section_title(title: &'static str) -> Div {
    div().flex().items_center().gap_3().h(px(26.)).child(
        div()
            .flex_1()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(brand::ink_3())
            .child(title.to_uppercase()),
    )
}

fn category_title(title: &'static str) -> Div {
    div()
        .mt_2()
        .text_xs()
        .font_family(brand::MONO_FONT)
        .text_color(brand::accent())
        .child(format!("// {}", title.to_lowercase()))
}

fn stat(title: &'static str, value: String, sub: String) -> Div {
    div()
        .flex()
        .flex_col()
        .child(label(title))
        .child(
            div()
                .font_family(brand::MONO_FONT)
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .child(value),
        )
        .child(div().text_xs().text_color(brand::ink_3()).child(sub))
}

fn stat_chip(text: String) -> Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(brand::line_2())
        .text_sm()
        .font_family(brand::MONO_FONT)
        .text_color(brand::ink_2())
        .child(text)
}

fn table_row(cells: Vec<(Div, bool)>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .text_sm()
        .border_b_1()
        .border_color(brand::line())
        .bg(brand::panel())
        .children(cells.into_iter().map(|(c, _)| c))
}

fn segments() -> Div {
    div()
        .flex()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded_md()
        .border_1()
        .border_color(brand::line_2())
}

fn segment(id: impl Into<ElementId>, text: &str, on: bool) -> Button {
    let b = Button::new(id).small().label(text.to_string()).selected(on);
    if on { b.primary() } else { b.ghost() }
}

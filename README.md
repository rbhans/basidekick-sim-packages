# BASidekick Building Sim

Fake buildings on BACnet/IP. Pick one, press **Start building**, and a BAS (Niagara, YABE, anything) can discover it. You get a site router plus one BACnet device per controller. Each controller runs a plain DDC sequence against a lite model of the building, so points move the way they would in a real building. Scenarios break things on cue.

Runs on macOS, Windows and Linux. Rust and GPUI (through `gpui-kit`), in BASidekick's Ink / Paper / hi-vis look.

## Get it

Every push builds all three platforms in GitHub Actions (`.github/workflows/build.yml`). The packages are attached to the run as artifacts, and a `v*` tag also puts them on a GitHub release. Each package holds the app and `basidekick-sim-cli`, the terminal runner.

| Platform | Package | First run |
|---|---|---|
| macOS (Apple silicon and Intel) | `basidekick-sim-<version>-macos-universal.zip` with `BASidekick Building Sim.app` | It's ad-hoc signed, not notarized: right-click → **Open** the first time. Allow local network access when asked. |
| Windows 10/11 (x64) | `basidekick-sim-<version>-windows-x86_64.zip` with `BASidekick Building Sim.exe` | It's unsigned: SmartScreen → **More info** → **Run anyway**. Allow it through Windows Firewall on private networks, or BACnet requests never reach it. |
| Linux (x86_64; `scripts/build-linux.sh` also builds on aarch64) | `basidekick-sim-<version>-linux-x86_64.tar.gz` | `./install.sh` puts it in `~/.local/bin` with a desktop launcher. The app needs X11 or Wayland and a Vulkan driver; `basidekick-sim-cli` needs neither. |

### Building it yourself

| Platform | Command | Needs |
|---|---|---|
| macOS | `scripts/build-mac.sh` (`--universal` for both chips, `--debug` for a quick bundle) | Rust, Xcode command line tools |
| Windows | `powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1` | Rust (MSVC) and Visual Studio Build Tools with the Windows SDK: GPUI compiles its DirectX shaders with the SDK's `fxc.exe`, so the Windows app has to be built on Windows |
| Linux | `scripts/build-linux.sh` | Rust and `pkg-config libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev libvulkan-dev libasound2-dev` (Debian/Ubuntu names) |

While developing, `cargo run -p basidekick-sim` works anywhere, and `cargo run -p basidekick-sim -- --package elementary-school --start` opens straight into a running building.

## Use it

1. Pick a building on the left, and the network interface plus UDP port it should answer on. The interface your computer routes through is picked by default.
2. Press **Start building**. The middle shows the clock, every zone and every controller. Click a controller to see its points and the faults you can throw at it.
   Left alone, it runs a normal day: no alarms or faults, zones in their band by opening time, and the scenarios panel says **ALL NORMAL**. Nothing goes wrong until you fire it.
3. Run scenarios from the right. The event log below them shows alarms, scenario starts and ends, and every write a BAS makes.

The clock starts at the real time of day. **Speed** runs it 1×–300× or pauses it; BACnet keeps answering while paused. **Weather** picks winter, spring/fall or summer. **Jump to** runs the building ahead to 6 AM, 8 AM, noon or 5 PM on the next weekday (or 10 PM), so it gets there the way it would have.

### In a terminal

`basidekick-sim-cli` runs one building without a window: on a server, in CI, or in a VM next to a station.

```bash
basidekick-sim-cli --list
```

```bash
basidekick-sim-cli --package elementary-school --port 47809 --speed 10 --scenario heat-wave
```

Options: `--package <id | file.toml>`, `--bind <ipv4>`, `--port <udp>`, `--speed <x>`, `--season winter|mild|summer`, `--scenario <id>` (repeatable). `--help` also lists this computer's interfaces and where your own packages go. It prints the log as it happens and a status line every 10 seconds. On macOS it sits inside the app at `BASidekick Building Sim.app/Contents/MacOS/basidekick-sim-cli`. On macOS and Linux, `basidekick-sim --headless` does the same.

## The buildings

| Package | What's in it | Devices |
|---|---|---|
| **Small Office** (`small-office`) | 5,000 sq ft, four packaged RTUs (2-stage DX, gas heat, economizers), restroom exhaust fan, lighting panel, meter | 8 |
| **Elementary School** (`elementary-school`) | 22,000 sq ft. Chiller and boiler plants, a VAV air handler with 10 hot-water-reheat boxes (classrooms carry CO2), gym and cafeteria RTUs, kitchen hood fan interlocked to the cafeteria unit | 20 |
| **Retail Store** (`retail-store`) | 12,000 sq ft, open 7 days. Two RTUs share the sales floor behind a glass storefront; stockroom with a receiving door; office RTU with electric heat | 8 |
| **Warehouse** (`warehouse`) | 40,000 sq ft, 30 ft clear. Four gas unit heaters, two roof exhaust fans, dock doors, office and break-room RTUs, two-shift schedule | 11 |

## On the network

Each building is one UDP socket and many devices, by BACnet routing:

- The **site controller** (`SITE-CTRL`) sits on BACnet/IP and routes to a virtual network: 2001 for the office, 2002 school, 2003 retail, 2004 warehouse. It carries outside air temperature, RH and dew point, the people count, the fire alarm, and `BLDG-OCC-MODE`.
- Every **controller** sits behind it at MAC 1, 2, 3… Device instances count up from the site's: the office is 1000 (site) and 1001–1007. The school uses 2000s, retail 3000s, the warehouse 4000s.
- It answers Who-Is (local, remote and global), Who-Is-Router-To-Network, ReadProperty, ReadPropertyMultiple (incl. ALL), WriteProperty, WritePropertyMultiple, SubscribeCOV (confirmed or unconfirmed) and Who-Has. No segmentation: oversized replies get Abort 4 and clients fall back to reading arrays element by element.

**Niagara:** add a BACnet/IP network on the port, discover. The router and network 2001 (etc.) are learned from the I-Am replies, and devices show up as `2001:1`, `2001:2`… If a station on the same computer already holds 47808, use 47809 for the sim and give the station a second BACnet/IP network on that port. If the port is shared with another program, the event log says so: that program may get the unicast requests instead. **Loopback** in the interface list keeps everything on this computer.

## How it behaves

- **Zones** are one thermal lump each: envelope and infiltration, sun on whichever way the glass faces, people, lights, plugs, process heat, open doors, and whatever air the HVAC delivers. CO2 comes from the people and leaves with outside air. No two rooms are quite alike.
- **Weather** follows a daily curve with day-to-day wander; the season sets the highs and lows. **People** come and go by profile (office, school, retail, warehouse).
- **Optimal start** brings the HVAC on early enough to have every zone in its band by opening. It starts about 12 minutes earlier for every degree the worst zone sits outside 70–74 °F, never less than the package's `hvac_lead_min` and at most 4 hours. The log says how early it started.
- **Controllers** run real sequences:
  - **RTUs:** PI heating and cooling loops, the economizer first, staged compressors with minimum on/off times and low-ambient lockout, and fan cycling at night.
  - **AHU:** fan speed on duct static, discharge air from one split-range loop (heat → economizer → cooling), a mixed-air low limit, night cycling on box calls, and a freezestat trip.
  - **VAV boxes:** pressure-independent flow reset and reheat. An airflow loop drives the damper against the duct static the AHU actually makes.
  - **Plants:** the loop temperature comes from coil loads, with outdoor reset on hot water. A plant is enabled on demand and proves its pump before the chiller or boiler starts.
  - Unit heaters, exhaust fans with interlocks, lighting relays and a meter with submeters.
- **Field devices act like field devices:**
  - Valves take 60 seconds to stroke and dampers 90.
  - Supply air reaches its sensor about 8 seconds after leaving the coil.
  - Wall sensors trail the room by a minute or two.
  - Airflow, duct static and supply air readings carry a little transducer noise.
- **A BAS can take over** like it would on a real job:
  - Write any output or setpoint at a priority, and the controller's program yields until you relinquish. The program's own value lives in relinquish-default.
  - Put a sensor out of service and write it, and the controller acts on your value.
  - `OCC-MODE` on each unit and `BLDG-OCC-MODE` on the site take Auto / Occupied / Unoccupied.
  - The point table marks commanded points `P8` and the like, out-of-service points `OOS`, and points in fault or alarm.

### Point names

Every unit uses the same short names, with the long name in the description:

- **Sensors:** `ZN-T` zone temp, `ZN-CO2`, `SA-T`, `MA-T`, `RA-T`, `OA-T`, `DSP` duct static, `AIRFLOW`.
- **Commands and statuses:** `SF-C` and `SF-S`, `CLG1-C` and `HTG1-C`, `OAD-O`, `CLG-VLV`, `HTG-VLV`, `RHT-VLV`.
- **Setpoints:** `OCC-CLG-SP`, `OCC-HTG-SP`, `UNOCC-*`, `SA-T-SP`, `DSP-SP`, `MIN-OA`, `MAX-FLOW`, `MIN-FLOW`.
- **Alarms:** `SF-FAIL`, `CMP-ALM`, `LT-DET`, `CH-ALM`, `B-ALM`.
- **Tuning:** `<LOOP>-KP` and `<LOOP>-TI` for every loop, plus `SAT-LOOP`, `CMP-MIN-ON`, `CMP-MIN-OFF`, `CMP-STARTS` and `HTG-DIFF`. See *Teaching controls* below.

## Teaching controls

Every loop can be tuned from the BAS, and the field devices have enough lag and dead time that bad tuning hunts the way it does on a real job. The defaults are conservative: a setpoint step settles with a few percent of overshoot.

| Controller | Loop | Points |
|---|---|---|
| RTU | Zone cooling and heating | `CLG-KP` `CLG-TI`, `HTG-KP` `HTG-TI` (gain in % per °F, integral time in seconds; 0 makes it proportional-only). Loop outputs: `CLG-DMD`, `HTG-DMD` |
| RTU | Compressor staging | `CMP-MIN-ON`, `CMP-MIN-OFF` (anti-short-cycle times), `CMP-STARTS` (starts in the last hour) |
| AHU | Supply air temperature | `SAT-KP` `SAT-TI`. `SAT-LOOP` shows the split-range output: below 0 heats, 0–50 is economizer, 50–100 is cooling |
| AHU | Duct static | `DSP-KP` `DSP-TI`. The output is `SF-SPD` |
| VAV | Zone cooling and heating | `CLG-KP` `CLG-TI`, `HTG-KP` `HTG-TI`. Loop outputs: `CLG-LOOP`, `HTG-LOOP` |
| VAV | Airflow | `FLOW-KP` `FLOW-TI` (gain in % per % of max flow). The output is `DMPR-O` |
| Unit heater | Thermostat | `HTG-DIFF` (on/off differential) |

Tuning points are writable values, not commandable: write them without a priority, and they stay until changed.

Exercises that work well (all on the Elementary School):

1. **Bump test.** Trend AHU-1 `SA-T`, `SA-T-SP`, `CLG-VLV` and `SAT-LOOP`. Step `SA-T-SP` from 55 to 58 °F and watch it settle.
2. **Too much gain.** Set AHU-1 `SAT-KP` 40 and `SAT-TI` 20, and step it again. The valve slams back and forth and supply air hunts about ±10 °F. Tune it back.
3. **Hunting static.** Set AHU-1 `DSP-KP` to 400. The fan surges, and every VAV airflow swings with it.
4. **A twitchy box.** Set VAV-104 `FLOW-KP` 6 and `FLOW-TI` 8. The damper hunts against its 90-second actuator.
5. **Proportional-only.** Set AHU-1 `SAT-TI` to 0, then step `SA-T-SP` 3 °F. It settles about 1 °F short (offset). Switching to proportional-only freezes the bias where it was, so the offset only shows once the setpoint or the load moves.
6. **Short cycling.** In the Small Office, the conference room's RTU-3 is oversized. Set its `CMP-MIN-ON` and `CMP-MIN-OFF` to 0 and `CLG-KP` to 200 on a summer afternoon. Runs shrink to about 3 minutes, supply air swings from 46 to 76 °F, and `CMP-STARTS` climbs from about 3 an hour toward 6.
7. **Find the fault.** **Mystery fault** (top of the scenarios panel) breaks something at random without saying what. Students work it out from alarms, trends and commands that disagree with statuses; **Reveal answer** shows what it was. In a terminal, `--scenario mystery` does the same, and `--show-answers` prints the answer for the instructor.

## Scenarios

Every building gets heat wave, cold snap, after-hours event, packed building, network outage, fire alarm and power outage. Each package adds its own, for example the school's assembly, lunch rush, chiller trip, boiler lockout, AHU fan VFD fault, stuck VAV damper and drifting sensor.

Weather scenarios fade in and out over their ramp. Timed ones end themselves, and **Stop** ends any of them early.

Open a controller for **Break it**: one-click faults that fit it, like a fan failure, loss of cooling or heat, a stuck OA damper, a drifting supply sensor, a loading filter, a failed or frozen zone sensor, or dropping offline. Taking the site controller offline takes the router, and everything behind it, with it.

## Your own buildings

A package is one TOML file. Drop it in your packages folder, or point `BASK_SIM_PACKAGES` at a folder. The sidebar's **Your own packages…** opens it, and `basidekick-sim-cli --help` prints it.

- **macOS:** `~/Library/Application Support/BASidekick/Building Sim/packages`
- **Windows:** `%APPDATA%\BASidekick\Building Sim\packages`
- **Linux:** `~/.local/share/basidekick-sim/packages` A file with a built-in's `id` replaces it. The four in `packages/` are the best reference. The shape:

```toml
id = "tiny-office"
name = "Tiny Office"
tagline = "One RTU, one room"
area_sqft = 1200
network = 2010
site_instance = 9000

[schedule]
weekday = ["08:00", "17:00"]
profile = "office"            # office | school | retail | warehouse

[[zone]]
id = "office"
name = "Office"
area_sqft = 1200
people = 8
exposure = 0.7                # 0 core … 1 lots of outside wall and roof
facing = "south"              # north | east | south | west | roof | core

[[equipment]]
type = "rtu"                  # rtu | ahu | vav | chiller_plant | boiler_plant | exhaust_fan | lighting | meter | unit_heater
id = "RTU-1"
zone = "office"
supply_cfm = 1200
cool_tons = 3

[[scenario]]
id = "rtu1-fan"
name = "RTU-1 fan fails"
category = "equipment"        # weather | occupancy | equipment | network | emergency
effects = [{ kind = "fail", equip = "RTU-1", part = "fan" }]
```

Effect kinds:

- `outdoor_temp { delta_f }`
- `people { factor, floor, zones }`
- `force_occupied`
- `heat_gain { zone, btuh }`
- `door_open { zone, cfm }`
- `fail { equip, part = fan | cooling | heating }`
- `stuck { equip, actuator = damper | oa_damper | cooling_valve | heating_valve, position }`
- `sensor { equip, point, mode = frozen | offset | fail, value }`
- `filter { equip, dp }`
- `offline { equip }` (`"*"` for all)
- `power_outage`
- `fire_alarm`

Optional fields and their defaults are in `crates/engine/src/package.rs`. A broken file shows its error in the sidebar instead of loading.

## Code

| Crate | |
|---|---|
| `crates/points` (`bask-points`) | The object database: devices, objects, priority arrays, the rules for writes |
| `crates/bacnet` (`bask-bacnet`) | BACnet/IP server with the virtual routed network, COV, and interface listing |
| `crates/engine` (`bask-sim`) | Packages, weather and schedules, zone physics, the controllers' sequences, scenarios |
| `crates/runner` (`bask-runner`) | The runtime that ties a sim to its server on a thread, and the terminal front end |
| `crates/cli` (`basidekick-sim-cli`) | The terminal binary, with no GPUI in it |
| `crates/app` (`basidekick-sim`) | The GPUI app |

```bash
cargo test --workspace
```

The tests run every building through a day in all three seasons, fire every scenario and quick fault, check BAS overrides and out-of-service writes, and speak raw BACnet to the server.

```bash
cargo run -p bask-sim --example day -- elementary-school summer chiller-trip AHU-1
```

That prints a building's day half-hour by half-hour, which helps when tuning a package.

Archivo Black and JetBrains Mono are bundled in `assets/fonts` under the SIL Open Font License.

## Limits

- It's a teaching and test fixture, not an engineering model. The physics is right in direction and roughly right in size and speed.
- The sequences are plain ones, not ASHRAE Guideline 36 (no static pressure or supply air resets, no demand-controlled ventilation).
- Each zone is one temperature, with no humidity or pressure relationships.
- There are no trend logs, schedules or intrinsic alarming (event-state stays normal; alarm points are binary inputs with in-alarm set).
- The vendor ID is fixed at 999.
- The macOS app is ad-hoc signed and the Windows exe unsigned, so each OS warns the first time.

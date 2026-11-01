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

While developing, `cargo run -p basidekick-sim` works anywhere. `cargo run -p basidekick-sim -- --package elementary-school --start` opens straight into a running building, and `--lesson sat-tuning` opens it set up for a lesson.

## Use it

1. Pick a building on the left, and the network interface plus UDP port it should answer on. The interface your computer routes through is picked by default.
2. Press **Start building**. The middle shows the clock, every zone and every controller. Click a controller to open its trend charts, its points and the faults you can throw at it.
   Left alone, it runs a normal day: no alarms or faults, zones in their band by opening time, and the scenarios panel says **ALL NORMAL**. Nothing goes wrong until you fire it.
3. Run scenarios from the right, or open the **Lessons** tab for guided exercises. The event log below them shows alarms, scenario starts and ends, and every write.

**Trend charts.** Each controller has a few ready-made charts: a reading with its setpoints, and the outputs driving it in a 0–100 % strip underneath, the way a loop trend looks in a BAS. Pick 15 minutes, an hour or a day. They fill from the moment the building starts.

**Changing points without a BAS.** Click a point's value to change it from the app, the way an operator would: setpoints and commands are written at priority 8 (with **Release** to hand them back), settings like tuning gains take the value, and sensors are taken out of service at the value you give (**Back in service** returns them). A BAS on the network sees all of it.

**Lessons.** Every building carries a few short exercises. **Set it up** puts the building back as built (every command released, every setting at its default), then moves it to the right season and time of day and starts any scenario the lesson needs. The lesson shows its steps with the controller's chart beside them, and **Show me** reveals what you should have seen.

The clock starts at the real time of day. **Speed** runs it 1×–300× or pauses it; BACnet keeps answering while paused. **Weather** picks winter, spring/fall or summer. **Jump to** runs the building ahead to 6 AM, 8 AM, noon or 5 PM on the next weekday (or 10 PM), so it gets there the way it would have.

### In a terminal

`basidekick-sim-cli` runs one building without a window: on a server, in CI, or in a VM next to a station.

```bash
basidekick-sim-cli --list
```

```bash
basidekick-sim-cli --package elementary-school --port 47809 --speed 10 --scenario heat-wave
```

Options: `--package <id | file.toml>`, `--bind <ipv4>`, `--port <udp>`, `--speed <x>`, `--season winter|mild|summer`, `--scenario <id>` (repeatable), `--lesson <id>` (sets the building up and prints the steps; add `--show-answers` for what to expect). `--help` also lists this computer's interfaces and where your own packages go. It prints the log as it happens and a status line every 10 seconds. On macOS it sits inside the app at `BASidekick Building Sim.app/Contents/MacOS/basidekick-sim-cli`. On macOS and Linux, `basidekick-sim --headless` does the same.

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
- It answers Who-Is (local, remote and global), Who-Is-Router-To-Network, ReadProperty, ReadPropertyMultiple (incl. ALL), WriteProperty, WritePropertyMultiple, SubscribeCOV (confirmed or unconfirmed), Who-Has, ReadRange, GetEventInformation and AcknowledgeAlarm. No segmentation: oversized replies get Abort 4 and clients fall back to reading arrays element by element, and ReadRange returns what fits with more-items set.
- **The building's clock:** every device's local date and time are the sim's, not your computer's. Schedules, trend log time stamps and alarm time stamps all run on it, fast clock and jumps included.
- **Schedule:** the site controller's `OCC-SCHEDULE` (schedule 1) runs the building's occupancy. It starts out as the package's hours, and a BAS can edit its weekly schedule, exceptions (dates, date ranges and week-and-day entries), default and effective period. Optimal start works back from whenever it next comes on, and the clock bar shows today's schedule when it differs from the opening hours. People come and go on their own pattern, whatever the schedule says.
- **Trend logs:** every controller keeps `TL-<point>` trend logs of its main readings (zone temperature, CO2 and humidity, supply air, duct static, airflow, mixed air, loop temperatures, outside air, kW): a sample a minute, a day deep. Read them with ReadRange or import them as histories. `Log_Interval`, `Enable` and `Record_Count` (write 0 to clear) are writable.
- **Alarms:** alarm points and zone temperature sensors report by BACnet intrinsic alarming: change of state for the alarm inputs, out of range (below 40 or above 100 °F) and sensor faults for `ZN-T`. Every controller has a notification class, `ALARMS`, that broadcasts unconfirmed event notifications on the local network out of the box, so a BAS listening for alarms just gets them. Edit its `Recipient_List` to send them somewhere specific, confirmed or not. Acknowledgments from the BAS show in the event log.

**Niagara:** add a BACnet/IP network on the port, discover. The router and network 2001 (etc.) are learned from the I-Am replies, and devices show up as `2001:1`, `2001:2`… If a station on the same computer already holds 47808, use 47809 for the sim and give the station a second BACnet/IP network on that port. If the port is shared with another program, the event log says so: that program may get the unicast requests instead. **Loopback** in the interface list keeps everything on this computer.

## How it behaves

- **Zones** are one thermal lump each: envelope and infiltration, sun on whichever way the glass faces, people, lights, plugs, process heat, open doors, and whatever air the HVAC delivers. CO2 comes from the people and leaves with outside air. No two rooms are quite alike.
- **Humidity:** people add water, leaks trade it with outside, and cooling coils wring it out of the supply air, more the colder they run. A wet coil leaves air close to saturated; a DX coil in muggy air spends up to a third of its capacity drying instead of cooling. Furnishings slow how fast a room's humidity moves.
- **Weather** follows a daily curve with day-to-day wander; the season sets the highs and lows. **People** come and go by profile (office, school, retail, warehouse).
- **Optimal start** brings the HVAC on early enough to have every zone in its band by opening. It starts about 12 minutes earlier for every degree the worst zone sits outside 70–74 °F, never less than the package's `hvac_lead_min` and at most 4 hours. The log says how early it started.
- **Controllers** run real sequences:
  - **RTUs:** PI heating and cooling loops, the economizer first, staged compressors with minimum on/off times and low-ambient lockout, fan cycling at night, and demand-controlled ventilation: the minimum outside air ramps from `AREA-MIN-OA` at 300 ppm under `CO2-SP` to `MIN-OA` at it.
  - **AHU:** fan speed on duct static, discharge air from one split-range loop (heat → economizer → cooling), a mixed-air low limit, night cycling on box calls, and a freezestat trip. Both setpoints reset by **ASHRAE Guideline 36 trim & respond**. Duct static starts at 0.5 in. and trims 0.05 in. every 2 minutes until boxes send pressure requests, then responds up. The warmest the supply air may run trims up 0.2 °F at a time until zones send cooling requests; below 60 °F outside the supply air rides that, above 70 °F it holds `SAT-MIN` to keep the air dry, and in between it's a straight line. Two requests are ignored (`REQ-IGNORE`). `SAT-RESET` and `DSP-RESET` turn each off.
  - **VAV boxes:** pressure-independent flow reset and reheat. An airflow loop drives the damper against the duct static the AHU actually makes. Each box sends Guideline 36 requests: cooling requests at 3 °F over setpoint (3), 2 °F over (2) or a maxed cooling loop (1); pressure requests under half its airflow (3), under 70 % (2) or with its damper wide open (1). `REQ-IMPORTANCE` scales a box's requests, and 0 ignores a rogue zone. Classroom boxes with CO2 run demand-controlled ventilation on their minimum airflow.
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

- **Sensors:** `ZN-T` zone temp, `ZN-CO2`, `ZN-RH`, `SA-T`, `MA-T`, `RA-T`, `RA-RH`, `OA-T`, `DSP` duct static, `AIRFLOW`.
- **Commands and statuses:** `SF-C` and `SF-S`, `CLG1-C` and `HTG1-C`, `OAD-O`, `CLG-VLV`, `HTG-VLV`, `RHT-VLV`.
- **Setpoints:** `OCC-CLG-SP`, `OCC-HTG-SP`, `UNOCC-*`, `SA-T-SP`, `DSP-SP`, `MIN-OA`, `MAX-FLOW`, `MIN-FLOW`, `CO2-SP`.
- **Resets and ventilation:** `SAT-RESET`, `SAT-MIN`, `SAT-MAX`, `DSP-RESET`, `DSP-MIN`, `DSP-MAX`, `REQ-IGNORE` and the request totals `CLG-REQ` and `PRESS-REQ` on the AHU; `CLG-REQ`, `PRESS-REQ` and `REQ-IMPORTANCE` on each box; `DCV`, `AREA-MIN-FLOW` / `AREA-MIN-OA` and `EFF-MIN-FLOW` / `EFF-MIN-OA`.
- **Alarms:** `SF-FAIL`, `CMP-ALM`, `LT-DET`, `CH-ALM`, `B-ALM`, all reporting through each controller's `ALARMS` notification class.
- **Schedule and trend logs:** `OCC-SCHEDULE` on the site controller; `TL-ZN-T`, `TL-SA-T` and so on, on each controller.
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

The **Lessons** tab walks through the best of these step by step. More that work well, all on the Elementary School:

1. **Bump test.** On a summer day, watch AHU-1's Supply air chart. Write `SA-T-SP` = 58 °F at priority 8 (the reset holds 55 when it's warm out) and watch it settle.
2. **Too much gain.** Set AHU-1 `SAT-KP` 40 and `SAT-TI` 20, and step it again. The valve slams back and forth and supply air swings about 10 °F peak to peak. Tune it back.
3. **Hunting static.** Set AHU-1 `DSP-KP` to 400. The fan surges, and every VAV airflow swings with it.
4. **A twitchy box.** On a winter day, disable AHU-1's `DSP-RESET` (so static holds 1.25 in.) and VAV-104's `DCV`, then set VAV-104 `FLOW-KP` 6 and `FLOW-TI` 8 and step `MIN-FLOW`. The damper hunts against its 90-second actuator.
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

[[lesson]]
id = "fan-failure"
title = "A fan that won't run"
level = "Beginner"            # Beginner | Intermediate | Advanced
goal = "Spot a failed fan from command against status."
setup = { season = "summer", time = "10:00", speed = 10, scenarios = ["rtu1-fan"], device = "RTU-1", chart = "Zone temperature" }
steps = [
    "Compare `SF-C` with `SF-S` on RTU-1.",
    "Watch `ZN-T` climb on the chart.",
]
expect = "Commanded on, no status, `SF-FAIL` in alarm, and the room warms."
```

Lesson text can name points in backticks; they show highlighted. `chart` is one of the controller's chart titles as the app shows them.

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
| `crates/points` (`bask-points`) | The object database: devices, objects, priority arrays and the rules for writes, schedules, trend logs and intrinsic alarming |
| `crates/bacnet` (`bask-bacnet`) | BACnet/IP server with the virtual routed network, COV, ReadRange, event notifications and acknowledgment, and interface listing |
| `crates/engine` (`bask-sim`) | Packages, weather and schedules, zone physics, the controllers' sequences, scenarios, lessons and trend history |
| `crates/runner` (`bask-runner`) | The runtime that ties a sim to its server on a thread, and the terminal front end |
| `crates/cli` (`basidekick-sim-cli`) | The terminal binary, with no GPUI in it |
| `crates/app` (`basidekick-sim`) | The GPUI app |

```bash
cargo test --workspace
```

The tests run every building through a day in all three seasons, check that left alone nothing alarms and every zone is in its band, fire every scenario and quick fault, set up every lesson (and check every point it names exists), check BAS overrides, out-of-service writes, the trim & respond resets, DCV and humidity, and speak raw BACnet to the server. `cargo run --release -p bask-sim --example audit` runs 120 untouched days and lists anything a BAS would flag; `--example tuning` runs the bump tests.

```bash
cargo run -p bask-sim --example day -- elementary-school summer chiller-trip AHU-1
```

That prints a building's day half-hour by half-hour, which helps when tuning a package.

## Limits

- It's a teaching and test fixture, not an engineering model. The physics is right in direction and roughly right in size and speed.
- The air handler takes Guideline 36's trim & respond resets, zone requests and demand-controlled ventilation, but not the rest of it: no outdoor airflow measurement, no fault detection rules, no building pressure.
- Each zone is one lump: one temperature and one humidity, with no pressure relationships. A very big space, like the warehouse floor, responds slowly because all its mass moves together.
- BACnet coverage is what a BAS needs for this building, not the whole standard: no Calendar objects, no trend log start/stop times or COV logging, no multi-state or command-failure alarms, no GetAlarmSummary or GetEnrollmentSummary, and confirmed event notifications are sent once without retries. The schedule can't be edited from the app, only from a BAS.
- The vendor ID is fixed at 999.
- The macOS app is ad-hoc signed and the Windows exe unsigned, so each OS warns the first time.

## License

The code is licensed under either of [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option. Contributions are taken under the same terms.

The BASidekick name, wordmark and icons in `assets/brand` are not covered by those licenses. Archivo Black and JetBrains Mono are bundled in `assets/fonts` under the SIL Open Font License.

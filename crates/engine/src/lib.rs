//! The BASidekick building simulator's engine: building packages, lite
//! physics, the DDC sequences every simulated controller runs, and the
//! scenarios you can throw at them. The BACnet face of it all is the shared
//! [`bask_points::PointDb`] each [`Simulation`] builds.

pub mod control;
pub mod equip;
pub mod history;
pub mod lesson;
pub mod package;
pub mod scenario;
pub mod sim;
pub mod snapshot;
pub mod world;
pub mod zone;

pub use history::{Role, Span, Trend, TrendLine};
pub use lesson::LessonDef;
pub use package::Package;
pub use scenario::{Category, ScenarioDef};
pub use sim::{Kind, Level, LogEntry, Simulation};
pub use snapshot::{DeviceSnap, PointSnap, ScenarioSnap, Snapshot, Write, ZoneSnap};
pub use world::Season;

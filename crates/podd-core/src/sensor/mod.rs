pub mod manager;
pub mod presence;
pub mod state;

pub use manager::{run, supervise};
pub use pod_proto::sensor::{SensorCommand, SensorPacket};

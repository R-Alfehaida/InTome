//! Text-to-speech: engines, voice profiles and the reading controller (port of prism/tts).

pub mod engine;
pub mod engines;
pub mod profiles;
pub mod reader;

pub use engine::{TtsEngine, VoiceInfo};
pub use engines::{available_engines, create_engine};
pub use profiles::{ProfileManager, VoiceProfile};
pub use reader::{ReadingController, ReadingState, ReadingTarget};

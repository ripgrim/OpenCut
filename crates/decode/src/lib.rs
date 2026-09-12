//! Portable media decode boundary: sources, frames, and the decoder driver socket.

mod driver;
mod frame;
mod source;

#[cfg(feature = "fake")]
mod fake;
#[cfg(feature = "mp4")]
mod mp4;

pub use driver::{DecodeError, Decoder};
pub use frame::{Frame, PixelFormat};
pub use source::{Source, SourceId, SourceStream, SourceStreamId};

#[cfg(feature = "fake")]
pub use fake::{FakeDecoder, FakeDecoderConfig};
#[cfg(feature = "mp4")]
pub use mp4::Mp4Decoder;
